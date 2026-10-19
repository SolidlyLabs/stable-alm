# A Claude agent that manages the pools: design plan, v4.1

Status: revised 19 Oct 2026 after two external reviews, a code-checked review, and the owner's
decisions. Phases 1 to 6 are built behind Settings → Experimental features (off by default; each
part has its own switch on top), with Liquidity Lens as placeholder data until its features arrive.
Phase 7 (the forward test) is elapsed time; phase 8 (the MCP server) is deferred to a later phase (owner's decision, 19 Oct 2026). Section 2a
records where the build differs from this plan.

v4 matches the plan to how the code actually works:
- a per-chain run lock, because capital is booked from wallet changes;
- replacing a stuck transaction on the emergency path;
- resetting strategy state on every switch;
- a heartbeat in the write gate;
- deeper data history;
- cost bounds passed to both executors of a move;
- per-pool cost attribution.

It also records new owner decisions:
- routine checks only hold or escalate;
- the loss limit is measured against holding;
- the issuer limit excludes the safe asset;
- API cost is shown but left out of the owner's score;
- the release decision is the owner's judgment;
- Claude only in v1.

v4.1 adds **Liquidity Lens** as an optional data source (section 6a), revised after Fable's review:
- executable quotes and depth for swap costs and the 24 h depth comparison;
- an on-chain reference price that never outvotes CEX prices (section 4);
- the traffic mix of pools, once Liquidity Lens exposes it beyond admins;
- keys per Stable ALM user, once Liquidity Lens can issue them.

Nothing depends on it: when it is missing or stale, the agent falls back to its own data.

Goal: anyone running Stable ALM can add their own Anthropic API key. The agent watches the market,
changes pool strategies, moves capital and adds pools, all inside limits that code enforces and the
agent cannot change. Execution and emergency responses stay deterministic.

## 0. Decisions taken

| Topic | Decision |
|---|---|
| Scope | **Full plan** in auto mode, within the limits below: tune and switch strategies, apply graphs the agent wrote (after the gate), move capital, and add pools whose tokens are already approved. A pool with a **token not yet approved always needs approval**. |
| Provider | **Claude only in v1.** A provider-neutral layer is deferred until other providers are actually compared. |
| Users | **Distributed**: other people install the app and use their own keys. Onboarding, key storage and the bring-your-own-key terms check are part of v1. |
| Safe asset | An **ordered list** (USDC, then USDT). The first one that is **healthy**, not implicated in the incident, and within its cost bound on a fresh quote is used. If none qualifies, the tokens are kept as they are and the user is alerted. |
| Exposure limits | **Settings with conservative defaults**; the user can change them, the agent cannot. The **issuer limit does not count the first healthy safe asset**. |
| Partial capital move | **Retry, then park**, and alert. |
| Stale approval | **Re-check before execution**; the plan expires if anything moved beyond its bounds. |
| Engine caps | **Every strategy**: at most 6 rebalances per pool per day, at most 100% of the pool's capital turned over per day, at most 0.5% of the pool's capital spent on execution per 7 days. When a cap is hit, the pool holds and alerts. All three are settings. |
| Daily capital movement | At most 20% of the account per day, counting **what leaves pools** (A→B once; A→B→A twice). **Deposits from Unallocated are deliberately not counted**: deploying fresh money is not churn. Emergency exits are exempt. |
| Routine checks | **Hold or escalate only**: a 4-hourly check cannot submit a plan; it can ask for a trigger review. |
| Pool loss limit | Measured **against holding the pool's own tokens**, so it fires on strategy failure, not on ETH's price. |
| Offline | **Accepted and documented**: no protection while the app is not running. |
| Overfit diagnostic (PBO) | **Advisory.** |
| API cost and the owner | The owner's $200/month of credits cost the owner nothing: API cost is **shown but left out of the owner's scored result**. For other users it is counted, and the UI shows it as a % of the account together with the edge needed to break even. |
| Release decision | **The owner's judgment**, informed by the scored report; no rule fixed in advance. An inconclusive test is reported as such. |
| Other users and auto | Allowed after they confirm they have read the owner's published results **and** their install passes the readiness checks. Advise is the default. |
| Web search | **On**, in daily and trigger reviews (routine checks cannot act, so they don't search). Bounded per review, with provenance recorded. |
| Agent memory | **Short notes, labelled unverified**, never policy. The decision journal is written by code. |
| Deterministic baseline | **Yes**, in the shadow test. |
| Forward test | **30 days**, frozen, extended when evidence is thin. |
| Budget (owner) | **$200 per month**, paced as a daily allowance with rollover; **25%** held back for priority reviews; hard stop at $200. |
| Budget (others) | **Must be set** before the agent starts. |
| Model | **Opus 5.5 everywhere**, effort set explicitly per review type, up to xhigh. |
| Alerts | **Desktop and Telegram** (approve/reject from a private chat with the linked user only). |
| Chains | **No bridging**; tokens and safe assets are per-chain contract addresses. |
| Experimental gate | **Every new feature of this plan sits behind Settings → Experimental features**, off by default: the Experimental tab, the Liquidity Lens client, and later the agent itself. Off means the app behaves exactly as without it. |
| Liquidity Lens before its features exist | **Placeholder data** (`llens.rs`): generated, deterministic, labelled `placeholder` everywhere, never decision-grade. Each feature is integrated for real when it arrives in Liquidity Lens. |
| Liquidity Lens | **Optional, never required.** Used for executable quotes, depth and an on-chain reference price. Every use has a fallback to Stable ALM's own data, and no safety check relies on it alone. It is an on-chain source, so it never outvotes CEX prices. Admin-only Liquidity Lens data (simulated fees, overrides, discovery rankings, and today the traffic mix) is not used. |

## 1. Why this shape

Claude handles the slower decisions: allocation and strategy selection. These match the controls
the app has (strategies, capital per pool, pools on or off). Execution and emergency responses stay
deterministic: they need bounded behaviour and must work without the model. Whether Claude improves
results is an empirical question for the forward test. The public AI-trading benchmarks are a source
of design ideas, not evidence of returns.

Responsibilities:
- **The model** proposes target allocations, strategies and pools, each with a thesis and a
  structured invalidation.
- **Code**:
  - turns targets into token amounts;
  - checks exposure on the projected state;
  - sets the execution bounds;
  - re-checks before every signature;
  - signs and sends;
  - runs an independent risk controller.

## 2. Architecture

```
 market data ─┐
 Liquidity Lens (optional, cached) ─┤
 pool events ─┼─► snapshot builder ─► Claude adapter (user's key) ── tools: bounded reads, research
 account     ─┘   (versioned, hashed)          │
                                               ▼
                    hold(reason)  |  submit_plan(plan)  (deduplicated; routine checks: hold/escalate)
                                               ▼
                    policy gate ── reject (reason back to the model, and to the journal)
                                               ▼
               approval: auto, or dashboard / Telegram (bound to the plan's hash)
                                               ▼
               execution coordinator: re-validate · reserve · one plan at a time · idempotent steps
                                               ▼
   ┌──────────────── per-chain run lock + write gate (gate.rs) ─────────────────────┐
   │ every writer: pool runs (engine), plan steps, emergency actions, manual moves,  │
   │ parking, claims. Held for a whole unit of work: balances before → act →         │
   │ balances after → capital book saved. Pre-sign check on each transaction:        │
   │ safety locks, emergency state, engine caps, controller heartbeat, spend limits  │
   └─────────────────────────────────────────────────────────────────────────────────┘
                                               ▼
                               chain (existing signer and pending-tx tracking)

 risk controller (wall-clock, every minute): stale data, price disagreement, token health,
 loss vs holding, gas, failed exits → emergency actions (next in line for the run lock)
```

**The run lock is required by the capital book.** `capital.rs` attributes every wallet change
during a pool's run to that pool: balances are read before and after, and the difference is booked.
That is only correct if nothing else touches the wallet meanwhile. Today the worker's single
`running` mutex gives that. With a coordinator, a risk controller and Telegram approvals on other
threads, it no longer would.

So:
- each **unit of work** holds a per-chain lock from its "before" balances to the saved book: a pool
  run, a plan step, an emergency action, a manual move, a claim;
- the units are serialised: a unit of work waits for the current one; an emergency is next in line
  and does not interleave;
- every writer that moves tokens books them through the same reserve → act → after_run → save path;
- the book's load-modify-save happens inside the lock (today only the file I/O is locked).

**The write gate** (`rs/src/gate.rs`) is the function each writer calls before signing. It denies
when:
- a safety lock covers the pool or chain;
- an emergency is active that the step does not belong to;
- an engine cap is reached;
- the risk controller's heartbeat is older than 5 minutes;
- the step exceeds the effective execution bounds.

### 2a. As built: one signer instead of a run lock

The build keeps the worker's cycle as the **only thread that signs**. The risk controller, the
execution coordinator, the scheduler's invalidation actions and Telegram never send a transaction:
they queue work for the cycle (a capital request tagged with its plan and cost bound, or a change to
pools.json) and wake the worker (`worker.request`). The capital book then sees every wallet change
inside a pool's run, as it does today, so no per-chain lock is needed. The cost is latency: an
emergency exit waits for the current cycle to finish (a minute or two), which the plan's emergency
timings allow.

The write gate is `rs/src/agent/gate.rs`, asked by the engine before every transaction and by the
claim path: with the risk controller on, a stale heartbeat denies everything; a lock or an
emergency on the pool denies deposits only. The engine caps are checked in the engine's own
pre-move checks. While a plan step runs on a pool, the worker runs that pool with the step's cost
bound (`exec::bounded`). Liquidity Lens is `rs/src/llens.rs`, the local cache `rs/src/cache.rs`.

Modules (`rs/src/agent/`, each also described at the top of its file):

| Module | Job |
|---|---|
| `claude.rs` | the Messages API adapter |
| `snapshot.rs` | builds the per-cycle snapshot |
| `tools.rs` | tool handlers |
| `plan.rs` | the plan object and its lifecycle |
| `policy.rs` | the policy gate |
| `exec.rs` | the execution coordinator |
| `risk.rs` | the risk controller |
| `journal.rs` | the decision record |
| `budget.rs` | API spend allowance and reservations |
| `notify.rs` | desktop and Telegram alerts |
| `sched.rs` | the scheduler |
| `llens.rs` | the Liquidity Lens client: key, quota, cache, staleness flags (section 6a) |
| `gate.rs`, `locks.rs` | the write gate and engine caps; safety locks and the emergency state |
| `perf.rs`, `features.rs`, `refdata.rs`, `exposure.rs` | performance against holding, pool features, exchange prices and funding, exposure |
| `history_sync.rs` | the deeper local history for managed and candidate pools |
| `sgate.rs` | the strategy gate's backtests |
| `review.rs` | one review: snapshot, model, tools, outcome |
| `shadow.rs` | the forward test's frozen baseline and scored report |

## 3. Plans: submission, lifecycle, execution

### Submission
- `submit_plan(plan)` takes one complete plan; `hold(reason, evidence_ids)` records a deliberate
  no-change. Both are strict tools.
- **Deduplication**: each call is keyed by `(review_id, tool_use_id)`, stored before acceptance.
- **One plan per review**; a resubmission is allowed only after a gate rejection, and it replaces
  the rejected one.
- **Routine checks** get only `hold` and `escalate(reason)`. `escalate` schedules a trigger review
  at the next allowed slot.

### The plan object
- `plan_id`, `review_id`, `snapshot_id`, `account_version`, `policy_version`, `prompt_version`,
  `model`, `plan_hash`.
- `changes`:
  - `strategy`: pool, `strategy_hash`, previous strategy hash;
  - `capital`: pool → target USD;
  - `add_pool`: chain, address, strategy, start USD;
  - `exit_pool`: pool.
- Per change:
  - thesis;
  - structured invalidation;
  - `evidence_ids`;
  - `est_switching_cost_usd` and `est_benefit` (labelled estimates).
- `requested_bounds`: only a request. **The effective bounds are the stricter of the user's
  execution settings and the plan's bounds**; approval never widens a setting.

### Execution settings (user-owned, code-enforced)

| Setting | Default | Meaning |
|---|---|---|
| Max normal execution cost | 15 bp | Of the USD amount leaving pools. Counts every swap leg (vs the reference price), gas, and the gas of failed attempts |
| Max emergency execution cost | 100 bp | Same measure, risk-controller exits only |
| Max quote age | 60 s | Re-quote when older |
| Max plan age | 6 h | Snapshot to the start of execution |
| Max price move since the snapshot | 1%, **in the units each step trades** | A pool's own price (token1 per token0) for in-pool steps; the token's price in the safe asset for swaps to and from it. An ETH pair does not expire just because ETH moved in USD |

The bounds must reach the code that swaps:
- a capital move **releases** in the source pool's executor (`Executor::release`, today
  `max_swap_cost_bps.max(10)`);
- it **deploys** in the destination pool's executor (funding swaps and the ratio swap, bounded by
  `slippage_bps` and `max_swap_cost_bps`);
- both receive the plan's effective bound for that step, and spending is checked across the plan's
  steps against its total.

### A capital move takes two phases
The source pool releases when it runs; the destination deploys when it runs. Each is a plan step in
its own unit of work. Before the burns, "fresh quotes for exact amounts" are estimates; each step
re-quotes what it actually holds. The plan stays `executing` between the phases, and its released
amount is reserved for the destination.

### States

```
proposed → validated → awaiting_approval → queued → executing → confirmed
               │               │              │          ├────► partially_completed
               ▼               ▼              ▼          ├────► failed
           rejected   expired / cancelled  expired      └────► superseded
```

### Execution rules
- **Re-validate before each step**:
  - balances, pending requests, reservations, pool status, safety state and mode;
  - fresh quotes;
  - projected exposure after the step and after the whole plan (section 5).
- **Pre-sign check** on every transaction (the write gate).
- **Emergency pre-emption**: the risk controller marks affected queued plans, and the unsubmitted
  steps of executing ones, `superseded`. Submitted transactions are reconciled first. The emergency
  then runs as the next unit of work. What a superseded plan released stays in Unallocated.
- **Reservation**: capital a plan moves is reserved until confirmed; plans run one at a time per
  chain; manual Add/Withdraw/Move also go through the coordinator.
- **Idempotency**:
  - steps are keyed `(plan_id, step_no)` and stored before sending;
  - after a restart, an `executing` plan resumes from its last confirmed step, after
    re-validation;
  - duplicated approvals are no-ops.
- **Partial failure**:
  1. retry up to 2 times within the effective bounds;
  2. park in Unallocated and mark the plan `partially_completed`;
  3. alert.
  - There is no automatic undo and no model call to improvise.
- **Approval** binds to `plan_hash`. These void pending approvals: a material change, turning the
  agent off, revoking Telegram, or changing a limit the plan relies on.

### Strategy changes reset the pool's strategy state
Node strategies keep their state per pool, not per strategy (`custom.rs`: exited, exit side,
timers, last move). Applying a strategy, or reverting one, therefore:
- **resets that state**: the pool starts as "not exited" with no timers;
- makes the new strategy decide on the same cycle (`engine::resolve_now`);
- records the old state in the journal.

Otherwise a new graph could inherit "exited" and wait for its own re-entry signal indefinitely.

## 4. Risk controller

Code only, outside every strategy graph, on a **wall-clock timer every minute**.

| Condition | Response |
|---|---|
| Reference data stale (no fresh source within 10 min) | Freeze new deployments and plans on the affected pools/chain |
| Price sources disagree beyond the token class's threshold (mid prices compared, not quotes; defaults: stables 30 bp, ETH vs WETH/LSTs 100 bp, others 200 bp). Sources come in two classes: **on-chain** (the pool, Liquidity Lens's reference price) and **off-chain** (CEX mids). A disagreement between the classes always freezes | Freeze, alert |
| Token health fails (stable off peg > 1%, LST discount beyond its limit vs its exchange rate) | Emergency exit of affected pools to the first healthy safe asset, else keep the tokens; alert |
| **Pool loss vs holding its own tokens** beyond a limit (−3% in 24h, −6% in 7d, settings) | Exit the pool to Unallocated, alert |
| Gas below the minimum | Freeze actions on that chain, alert |
| An exit failed | Retry under the emergency limits, then alert |

- **Pool loss** is the pool's marked result against simply holding the two tokens it held at the
  start of the window. It captures strategy failure: bad ranges, IL, costs. ETH's own price risk is
  handled by the exposure limits and the strategies' exits, not by this stop.
- **Heartbeat**: the controller writes a heartbeat every evaluation. The write gate denies every
  writer when it is older than 5 minutes. A stopped controller cannot act, so the gate does.
- **Stuck transactions**: today a pending transaction blocks every new one on its chain until it is
  mined, and is re-broadcast unchanged (`chain::reconcile_pending`). For emergencies:
  - after 2 minutes pending, the risk controller **replaces it at the same nonce** with a higher fee
    (the same transaction, or a zero-value self-transfer that cancels it), within the emergency gas
    limit;
  - the normal path keeps today's behaviour.
- **Latency bound** (stated and tested): an emergency acts within the current unit of work (at most
  one pool pass), plus pending resolution (at most about 4 minutes with replacement), plus its own
  transactions.
- **Safe-asset selection**: the first approved destination that passes its health check, is not
  implicated, and is reachable within the emergency cost bound. Otherwise keep the tokens and alert.
- **Emergency limits**: the emergency cost bound, and at most one emergency exit per pool per hour.
  They ignore the agent's action limit, the movement limit and the no-reversal rule. They are bound
  to the affected pools.
- **Safety locks**: no deposits, plan steps or engine deployments on a locked pool or chain;
  withdrawals and exits are allowed. A lock lifts when the condition clears and the user confirms,
  or after the user's timer.
- **Liquidity Lens in the controller** only adds evidence:
  - **It is not independent of the pool.** Liquidity Lens prices by routing through replicas of
    the same on-chain pools, including ours, so the pool and Liquidity Lens agreeing says nothing
    about CEX. A CEX move that the DEXes have not followed yet (thin arbitrage on Base) is exactly
    the early warning the freeze is for. So Liquidity Lens never outvotes a CEX price; it is used
    to tell **which on-chain venue is off** when the pool and the wider DEX market disagree.
  - **Input**: the keyed `/ext/oracle/reference` (a time-weighted median with a status and reasons:
    deviation, stale, no route), not an instantaneous quote. A status other than ok means no
    reading.
  - **The quote token**: Liquidity Lens prices everything in the chain's canonical stable (USDC
    on Base), which is also our first safe asset, and never prices that token itself. So it gives
    no reading for USDC, and its prices are converted to USD with the CEX price of USDC before any
    comparison. When USDC itself fails its health check, Liquidity Lens is dropped from every
    check on that chain.
  - **Cadence**: the controller has its own cache, one batch for the managed tokens every 5
    minutes (default), separate from the reviews' cache. The per-key daily cap is sized from it
    (section 6a).
  - A widening gap between spot and executable price over 24 h raises a **token-health
    warning**: an alert and a trigger review, never an exit by itself. A history still filling
    (`filling: true`) is incomplete and raises nothing.
  - Its outage never fires a condition, and never satisfies one.
- **A suspect source** (a feed that disagrees with every other source in its class, or fails its
  own checks): it is alerted, it expires after 1 hour unless re-confirmed, and its owner is the
  controller. Token health never runs on zero sources: with no healthy off-chain price for a
  token, the token's pools are frozen for new deployments until one returns.
- **Strategy invalidations** are a separate layer (the agent's thesis); safety conditions never are.
- **Offline**: no protection while the app is not running. It is documented, and the dashboard shows
  "protection last active" and gaps; gaps go in the journal.

## 5. Exposure model, limits and precedence

Each token has a classification (settings, defaults for the known tokens), keyed by per-chain
contract address:
- underlying: USD, ETH, BTC, other;
- issuer;
- relative-price risk: LST discount, stablecoin peg.

| Limit (setting) | Default |
|---|---|
| ETH-family exposure | ≤ 50% of account |
| BTC-family exposure | ≤ 25% |
| Other volatile tokens, total | ≤ 10% |
| Any single stablecoin issuer, **not counting the first healthy safe asset** | ≤ 75% |
| LST discount exposure | ≤ 40% |
| Any single pool | ≤ 75% |
| Any single chain (once several are used) | ≤ 100% |
| Exit capacity (share of non-USD exposure that can reach a healthy safe asset within the normal bound; measured with quotes at the actual amount and at 2×, from the aggregator or Liquidity Lens's batch quotes. Liquidity Lens's depth ladder (1%/2% bands) is too coarse for a 15 bp bound and is used only for the 24 h comparison) | ≥ 90% |
| Capital leaving pools per day by plans (deposits from Unallocated not counted; emergencies exempt) | ≤ 20% of account |
| A new pool's starting capital | ≤ 5%; grows only after 14 days of results |
| Agent actions per pool per day | ≤ 3; no reversal of its own change within 12 h unless its invalidation fired |

Excluding the safe asset from the issuer count keeps the defaults consistent. Otherwise an all-USDC
start, USDC exits and a USDC/USDT pool would leave the account permanently "in breach". If the first
safe asset fails its health check, the next one takes its place: the excluded asset is always the
current first healthy one.

**Engine caps** (every strategy, settings):

| Cap | Default | How it is built |
|---|---|---|
| Rebalances per pool per day | ≤ 6 | Builds on the existing `max_actions_per_day`, which counts logged rebalances, top-ups and withdrawals |
| Capital turned over per pool per day | ≤ 100% | Needs per-pool execution-cost attribution (phase 1); today's ledger is per chain |
| Execution cost per pool, rolling 7 days | ≤ 0.5% of its capital | Same per-pool attribution |

When a cap is hit, the pool holds and alerts. Emergencies are not capped by these.

**Precedence**:
1. **Emergency and partial-completion states may breach allocation limits.** The breach is
   recorded and alerted, and new deployments that add to it are blocked until it is back within
   limits.
2. **Existing breaches** (from prices or the user): changes that worsen them are blocked; changes
   that reduce them are allowed, step by step.
3. **Normal plans** must satisfy every limit on the projected state after each step and after the
   whole plan; exits are ordered before deposits.
4. Effective bounds are always the stricter of user settings and the plan's bounds.

## 6. Accounting and data (phase 1, before any agent)

**Accounting** (the primary record):
- marked account equity, adjusted for cash flows, per pool and in total; capital transfers are not
  profit;
- explained by fees, rewards, the inventory/price effect and execution costs. **Execution costs are
  attributed per pool** (swaps and gas per transaction, tagged with the pool or plan that caused
  them). The engine caps and the gate's switching costs need it;
- IL and LVR are separate diagnostics, never subtracted together;
- API cost is recorded per review and per plan. In the owner's scored result it is shown but not
  deducted; for other users it is deducted.

**History depth**:
- the strategy gate's data needs about **45 days** of swaps per pool: research up to 14 days ago,
  validation on the last 14 days, plus lookbacks up to 370 h;
- this applies to managed pools and about 8 candidates;
- **sources by age**:
  - **Solidly pools, any age**: the Solidly UI data processor. Its public history
    (`solidly.com/api/history`, already read by `history.rs`) gives every pool event per chain and
    day, and its hourly history (`apr_hourly.json`) gives fees, volume and emissions. RPC fills only
    the minutes the processor has not synced yet;
  - **other DEXes' pools, the last 14 days only**: Liquidity Lens. It keeps 2 weeks and no more
    (its disk is the limit);
- the worker keeps about 14 days today (`replay_days.max(13) + 1`). Phase 1 extends the local store
  to 45 days for those pools from the processor, and measures the download size and time per chain;
- candidates are synced on a slower schedule than managed pools;
- **a consequence for the gate**: only Solidly pools have the 45-day window. A comparison that
  involves another DEX's pool (competing liquidity, a pool on another DEX as a reference) uses at
  most 14 days, and the snapshot labels it so.

**Local cache first** (every external source):
- **Download on first use, then top up.** The first time a pool, token or series is needed, its
  history is downloaded once and stored under `data/`. Later reads ask only for what is newer than
  the last stored row. This is how `history.rs` already treats the data processor: a closed day is
  written once and never fetched again.
- **Every reader goes through the cache**: the engine, the risk controller, the snapshot builder,
  the agent's tools and the backtester. A tool call the cache can answer makes no request.
  Backtests and the strategy gate read local data only.
- **Closed data is kept, open data expires**:

  | Data | Source | Kept / refreshed |
  |---|---|---|
  | Pool events of a closed UTC day | Solidly UI data processor | kept; never re-fetched |
  | Hourly fees, volume, emissions | data processor | closed hours kept; topped up hourly |
  | Spot vs executable and depth history, closed buckets | Liquidity Lens | kept locally **beyond Liquidity Lens's 2 weeks**, so a longer series builds up on disk |
  | Traffic mix, closed hours (once R5 exists) | Liquidity Lens | kept; topped up hourly |
  | Reference price, quotes | Liquidity Lens, aggregator | not reused beyond their age limit (controller 5 min, quotes 60 s) |
  | CEX mids | public REST | 1 min |
  | Funding and OI | Hyperliquid | own hourly snapshots, kept |
  | Token metadata, pool lists, fee and tick spacing | chain, processor | kept; re-checked daily |

- **Kept across restarts.** A restart reads the cache instead of downloading again.
- **Checked on the way in**: a damaged or partial file is dropped and only that piece is
  downloaded again. A source that fails its sample check against the chain is not cached.
- **Pool events are never pruned by age while a backtest can use them.** Backtests go back up to
  2 years (`backtest.rs`, `MAX_DAYS` = 730), so a pool's events are kept for as long as the pool is
  managed, a candidate, or used by a saved strategy or backtest, up to that limit. Events older
  than the backtest limit, and those of a pool nothing uses any more, can be pruned, and only
  after the user confirms. The store prunes nothing today, and stays that way by default.
- **Other series**: kept as long as the event store keeps their pool; series with no pool (CEX
  mids, funding and OI, token prices) are kept for 2 years.
- **Size**: the size per source and per pool is shown on the dashboard, with a warning when the
  disk runs low.
- **Measured**: requests per source per day and the cache hit rate are on the dashboard and in the
  scored report.

**Snapshot data**:

| Area | Fields |
|---|---|
| Position geometry | current tick, lower and upper ticks, tick spacing, distance to each boundary (ticks, %), liquidity |
| Inventory | token quantities per pool and the resulting exposure |
| Activity | hourly volume and fees for 7 days |
| Data quality | per field: observed-at, retrieved-at, source, complete/partial, stale |
| Execution | fresh quotes at plan amounts (route, block, gas, expiry) |
| Operational | pending transactions, reservations, failed actions, locks, breaches, open plans, cap usage |
| Grounding | per pool: the gate's latest "vs current strategy after switching cost" figure; the realised cost and outcome of the agent's past changes there |

**Features**:

| Tier | Data | Source |
|---|---|---|
| 1 | Regime features: realised vol (1h/24h/7d), ATR in ticks, z-score vs 72h mean, trend (ADX, EMA slope), time out of range | Existing bars, `ta.rs` |
| 1 | Reference prices: CEX mids per token; cbETH/ETH exchange rate | Public REST; on-chain rate (Base source to confirm) |
| 1 | Token health: peg deviation, LST discount vs its rate | From the reference prices |
| 1 | Gas and the $ cost of a rebalance | Existing readings |
| 1 | Swap-cost scenarios (below) | Aggregator quotes, backtester |
| 1 | Per-pool performance (7d/30d), incl. per-pool execution costs | The accounting above |
| 2 | Funding and open interest, ETH and BTC | Hyperliquid `metaAndAssetCtxs`; own hourly snapshots for 24h changes |
| 2 | Competing liquidity near the price | Stored mint/burn events |
| 2 | Executable quotes and depth (±1%, ±2%) per token; on-chain reference price; spot vs executable history | Liquidity Lens price oracle (section 6a) |
| 3 | Pool traffic mix: aggregator, bot and organic share of volume, hourly | Liquidity Lens volume attribution, admin-only today (request R5) |
| 3 | Competing pools across DEXes, ranked ranges, value of added depth | Liquidity Lens modules still planned (section 6a) |
| 2 | Emissions and bribes, next epoch | Solidly UI data |
| — | Market-wide liquidations | Unresolved; drop unless a source is found |

**Swap-cost scenarios**:
- *now*: a fresh quote for the amount;
- *thin*: a quote for twice the amount, its bp cost applied to the amount (about half the depth on
  concentrated liquidity). With Liquidity Lens, its batch quotes give the 2× cost directly, and
  its depth history shows how the depth for that size compares with 24 h earlier;
- *delayed*: the shadow evaluator re-quotes 10 minutes after the decision, outside the review's
  time limit;
- *historical replays*: in-pool legs are priced by the pool's own swaps; legs to and from USDC use
  the measured round trip, with a floor of 1 bp and a thin case at 3×.

**F/LVR**:
- hours are classified: with swaps, without swaps (valid; fees 0), or missing;
- coverage below 80% makes it "unknown";
- fees 0 and LVR 0: "no activity"; fees > 0 and LVR 0: "no measurable LVR";
- otherwise fees ÷ LVR, capped at "> 50";
- the admission threshold (2) is a setting.
- **Traffic mix beside it** (once available, request R5): a pool whose volume is mostly bots tends
  to pay LPs less than its fee APR suggests. The bot share is shown next to F/LVR as context. It is
  not a second admission rule until the forward test shows it adds anything.

## 6a. Liquidity Lens (optional data source)

Liquidity Lens is the owner's live service. It runs exact replicas of the DEX pools on Optimism,
Base, Arbitrum and Sonic, and prices tokens by what a trade of a given size would actually get.
Its prices come from the same on-chain pools Stable ALM trades in, so it is an **on-chain source**,
not an independent one (section 4). Every price is in the chain's canonical stable (USDC on Base),
which it never prices itself.

**Used in v1** (in Liquidity Lens's code today; its product doc still calls the oracle "planned"):

| Data | Endpoint | Used for |
|---|---|---|
| Quotes by size and direction with route proof; batch | keyed `/ext/oracle/quote`, `batch` | swap-cost scenarios, exit capacity at the actual amounts, a cross-check of the aggregator quote |
| Reference price (time-weighted median, with status and reasons) | keyed `/ext/oracle/reference` | the on-chain reference in the risk controller |
| Depth ladder (largest size within 1% and 2%) and its history | keyed `/ext/oracle/depth`, `history` | the 24 h depth comparison and the token-health warning |

**Not usable yet**:
- **Traffic mix** (volume attribution) is an admin-only module: it needs a whitelisted admin
  wallet, and API keys cannot reach it. It is used only once Liquidity Lens exposes it with an
  entitlement (request R5). It keeps 14 days of hourly per-pool data and 400 days per DEX.
- **The free public oracle** serves already-published rows (a full pass takes about 20 minutes on
  Base, and a row can be up to an hour old). It allows 1 request per second per IP. It is shown on
  the dashboard but never used for a time-sensitive check.

**Wanted later** (planned in Liquidity Lens; requested in `LLENS_REQUESTS.md`): competing pools and
their in-range liquidity across DEXes; ranked ranges with expected APR; the value of added depth
per pool; candidate pairs with demand.

**Rules**:
- **Optional**: every field from Liquidity Lens carries the same data-quality flags as the rest of
  the snapshot, including its own `stateAgeMs`. Staleness thresholds are per endpoint (default:
  keyed quotes and reference 10 minutes; depth history 2 hours). A history with `filling: true` is
  incomplete. When data is missing or stale, the snapshot says so, and the agent works from Stable
  ALM's own data.
- **Not for execution**: a swap is still quoted and sent through the Solidly UI aggregator. A
  Liquidity Lens quote is a cross-check and a cost estimate, never what gets signed.
- **Not alone for safety**, and never outvoting a CEX price: section 4.
- **History**: Liquidity Lens keeps 2 weeks of history and will not keep more (disk space). Longer
  history of Solidly pools comes from the Solidly UI data processor (section 6), and is stored
  locally.
- **No admin data**: simulated fees, overrides, scope decisions and discovery rankings stay out
  unless the owner exposes them deliberately.

**Keys and quota**:
- **Issuance does not exist yet.** Today a key belongs to an organisation, and only an operator
  creates it and sets its limits. Until Liquidity Lens issues keys to wallet users (request R1),
  the owner's install uses a key the owner creates, and other users run without Liquidity Lens.
- The target design:
  - one Liquidity Lens organisation for Stable ALM, entitled to the oracle, read-only;
  - a key per user, issued after signing in with the wallet (SIWE) from the app;
  - the key is stored like the other keys and sent only in a header.
- **Limits**: per-key limits only narrow the organisation's cap, which all keys share. So the sum
  of the per-key daily caps is kept below the organisation's cap. Each key's cap is sized from:
  - the controller's batch every 5 minutes (288 a day, one quote per token);
  - one batch per review;
  - 10 uncached tool calls per review.
- **Cache first** (section 6): history is downloaded on first use and only topped up after.
- **Metering**: the gateway meters a call before it checks that a replica is available, so an
  outage still uses quota. `llens.rs` stops calling after the second failure in a row and backs
  off (5, 15, then 60 minutes). It never retries in a loop.
- **Deadline**: every keyed call carries `x-compute-deadline`, an absolute time in milliseconds
  compared with the server's clock. `llens.rs` computes it from the server's `Date` header plus
  the call's budget, so a slow local clock does not fail every call.
- **The test button** tells apart clock skew, quota, an unauthorised key and an outage.
- **Calls are serialised**, and public endpoints are called at most once per second.

**Privacy**: signing in links the user's wallet to their key. The usage ledger records each key's
daily requests and quotes, and the queries show what sizes the agent is pricing, so the
Liquidity Lens owner can see each user's activity. Onboarding says so, and the user can leave
Liquidity Lens off.

## 7. The snapshot

- One builder, the same order every time, saved with its hash.
- Every price series gives its absolute anchor (last close in USD and ticks), token order and units.
  Candles are % from the anchor, oldest → newest, labelled.
- Blocks:
  1. header (time, review type and reason, mode, limits, budget);
  2. portfolio and exposure (incl. breaches);
  3. managed pools, with grounding;
  4. about 8 candidates in detail, the rest one row each;
  5. market regime and token health;
  6. open plans, locks, cap usage, pending actions;
  7. memory (the last 10 decisions and outcomes, then agent notes, unverified).
- Size: 15–30k tokens.
- Cache breakpoints after the tools and system prompt, and after the snapshot. The hit rate is
  measured and shown.

## 8. Tools (bounded)

| Group | Tools | Bounds |
|---|---|---|
| Read | `get_portfolio`, `get_pool`, `list_pools`, `get_candles`, `get_indicator`, `get_journal`, `get_limits` | 500 candles per call, 50 journal entries, 40 tool calls and 10 minutes per review |
| Market (Liquidity Lens, when enabled) | `get_quotes(token, sizes, side)`, `get_depth_history(token, size, side)`; `get_traffic(pool)` only once request R5 exists | Served from the review's cache when possible; at most 10 uncached calls per review, counted against the user's key quota |
| Research (daily and trigger reviews) | `run_backtest`, `get_backtest`, `check_overfit`, `optimize`, `quote_swap`, `list_library` | One agent research job at a time, lower priority, own thread pool; backtests and optimizers see data up to 14 days ago |
| Web (daily and trigger reviews) | Claude's server-side web search, version pinned (currently `web_search_20260209` or later, chosen at build) | A code-owned counter per review (default 5); each call passes `max_uses` = what is left. Sources are recorded as evidence |
| Decide | `hold`, `submit_plan` (strict); `escalate` (routine checks only); `write_note` (≤ 500 characters, labelled, kept 30 days) | `submit_plan` is the only call that changes anything; routine checks cannot call it |

Strict schemas guarantee the shape only; Rust validates every bound and the graph semantics. There
are no tools to send tokens out, or to change settings, limits, the wallet, keys, RPCs or the
approved-token list.

## 9. Strategy gate

A recent-history validation gate, **reused across reviews**: it guards against bad strategies; it is
not an untouched holdout. Claims about performance belong to the forward test.

- **Semantics and runtime limits**: local evaluation on recent bars, nodes within their ranges, a
  supported bar size, no always-exit or never-trade graphs, at most 60 nodes, a bounded evaluation
  cost per bar, lookback at most 370 h. The engine caps bound it once it runs.
- **Windows** (this removes a contradiction with "don't chase winners"):
  - comparators are evaluated over the **longest window the data allows** (up to 45 days);
  - the last 14 days must also pass as the required out-of-sample slice.
  - A strategy that only shines in the last two weeks fails the long window. One that only
    worked earlier fails the slice.
- **Data split**: research tools see data up to 14 days ago; the agent gets pass/fail and the
  reason category, not the slice's numbers.
- **Comparators** (after costs, under every cost scenario): beat **holding**, and for a replacement,
  beat **the pool's current strategy** by more than the estimated switching cost.
  - On stable pairs the switching hurdle (up to 15 bp of the capital moved, plus the reset of a
    strategy's state) is large next to two weeks of fee differences, so most replacements will fail.
    The prompt says so, and the snapshot gives the current figure, so the agent does not spend
    attempts on marginal switches.
- **Walk-forward efficiency**: per equal slice, after-cost excess return over holding in-sample
  (best variant on earlier slices) and out-of-sample (next slice). WFE = Σ out ÷ Σ in. In-sample
  excess below 0.1% of capital fails ("no in-sample edge"); WFE below 0.5 fails.
- **PBO** (advisory): on a 32-variant neighbourhood built by the gate (fixed seed), stored with the
  variant set.
- **Search history**:
  - every candidate tried on a pool is recorded, across all families;
  - more than 5 in 7 days is flagged "found by search" (a disclosure, not a guarantee);
  - in auto mode, an agent-written graph with more than 5 attempts needs approval.

## 10. Prompt specification and review outcomes

Versioned files: `rs/prompts/agent_system_v1.md`, `rs/prompts/plan_schema_v1.json`. The version is
in every journal record.

- **Objective**: maximise **incremental net value relative to keeping the current setup**, after
  swaps and gas, within the limits. For other users, API cost is part of the net value; for the
  owner it is shown but not deducted.
- **Holding is the default**: when the evidence does not clearly favour a change, call `hold`.
- **Do not chase recent winners**. Use the grounding figures (the gate's vs-current result, and the
  realised cost and outcome of past changes on the pool) before proposing a switch.
- **Each change** gives: evidence ids, estimated switching cost and benefit (labelled estimates), a
  thesis, a structured invalidation and an expiry.

**Invalidations** (machine-checked):

```json
{"metric": "zscore_72h|price_usd|f_lvr_7d|net_return_vs_hold_7d|out_of_range_hours|...",
 "source": "pool|cex|ledger", "op": "<|>", "threshold": -2.5, "persist_minutes": 60,
 "action": "exit_to_safe|revert_strategy|withdraw_to_unallocated|alert", "note": "text"}
```

`revert_strategy` restores the exact previous strategy (by hash) and resets the pool's strategy state
(section 3). If that strategy is no longer admissible, the pool exits to the safe asset and alerts.

**Review outcomes** (recorded separately):

| Outcome | Meaning |
|---|---|
| `hold_decision` | A deliberate `hold` |
| `escalated` | A routine check asked for a trigger review |
| `plan_submitted` | A plan passed the gate |
| `plan_rejected` | Every submission was rejected |
| `incomplete` | Ended without `hold`/`submit_plan`/`escalate`: out of tool calls, time, or `max_tokens` (thinking counts toward it) |
| `refused` | The model refused |
| `api_error` | The API failed after retries |
| `budget_skipped` | Not run: no allowance |

Only `hold_decision` counts as restraint.

**User policy**: free text is converted in the UI into structured limits, confirmed by the user; only
structured limits bind. Agent notes are labelled unverified, capped, and expire.

**Untrusted input**: web results, token names and pool metadata are data. The gate limits damage but
does not make injection harmless; provenance, turnover limits and caps apply.

## 11. Scheduler

- **Review types** (effort always set explicitly; Opus 5.5 defaults to `medium` otherwise, and `low`
  is the floor because thinking cannot be turned off):

  | Type | Schedule | Effort | Can do |
  |---|---|---|---|
  | Daily review | 16:00 UTC | high | hold or plan |
  | Routine check, managed pools only | every 4 h | low | hold or escalate |
  | Trigger review | when triggered | medium; xhigh when severe | hold or plan |

- **Triggers**, each with its severity:

  | Trigger | Severity |
  |---|---|
  | A pool out of range longer than its strategy expects | normal |
  | A strategy invalidation fired | normal |
  | Realised vol above its 30-day 95th percentile | normal |
  | A funding or OI jump (\|Δ funding\| > 3× its 30-day stdev; OI ±10% in 24h) | normal |
  | A candidate's F/LVR crossing the admission threshold | normal |
  | A capital change by the user | normal |
  | An escalation from a routine check | normal |
  | A plan partially completed, superseded or failed | severe |
  | A risk-controller action (a follow-up review) | severe |
  | An engine cap hit | severe |

- **Debouncing**: at most one normal trigger review per hour; severe ones at most every 15 minutes.
- **Coalescing**: triggers during a review or the debounce window are merged into the next review,
  which lists them all.
- **One review at a time.** A routine check within 30 minutes of the daily review is skipped.
- **Without the agent** (no API, no budget, off): the engine, the caps and the risk controller keep
  running; reviews are recorded as skipped or failed.

## 12. Evaluation and release

1. **Development shadow** on the dry capital book. It is for tuning and proves nothing.
2. **Freeze**: prompt, model, effort rules, tools, limits, gate and cadence are hashed. A material
   change starts a new version.
3. **Scored forward test, 30 days**, against shadow baselines from the same capital:
   - the static setup;
   - holding;
   - the plain ladder;
   - the deterministic rules manager.
4. **Shadow costs are charged.** Dry-run releases today move tokens "as they are" at no cost. The
   shadow books charge every capital move and strategy switch at the quote-based cost (the *now*
   quote, plus the *delayed* re-quote in a second line). Without that, the agent flatters itself.
   Fee share uses the backtester's elasticity model. Everything is labelled as an estimate.
5. **Evidence counts**, all reported:
   - completed reviews by outcome;
   - plans submitted, rejected and executed;
   - changes made;
   - days each strategy was in force.
6. **Extension**: if fewer than 20 completed reviews ended in a decision (hold or plan), or fewer than
   5 plans executed, extend by 15 days, up to 90. Holds count, so trading is never needed to satisfy
   the rule. At 90 days the report says **inconclusive** with all counts.
7. **Small live stage**, manually approved, on a capped amount (e.g. 10% of the account): shadow
   estimates vs actual fills.
8. **Scored report**:
   - net performance vs each baseline after swaps and gas;
   - API cost alongside, deducted in a separate line for other users' context;
   - drawdown;
   - operational reliability;
   - evidence counts;
   - a plain statement of what 30 days can and cannot show.
   - It is published with the release.
9. **Release decision**: the owner's judgment from the report; no rule fixed in advance.
10. **Other users' auto mode**: they confirm they read the report, **and** their install passes the
    readiness checks:
    - fresh data feeds and reference prices;
    - gas above the minimum;
    - limits and approved tokens reviewed;
    - a budget set;
    - a test alert acknowledged;
    - at least 3 days in advise mode without `api_error` or `incomplete` streaks.

## 13. Model, effort and budget

- **Opus 5.5** (`claude-opus-5-5`):

  | Billing item | Price per million tokens |
  |---|---|
  | Input | $4 |
  | Output | $20 |
  | Cache write, 5 minutes | $5 |
  | Cache write, 1 hour | $8 |
  | Cache read | $0.20 |

  Web search: $10 per 1,000 searches, with results billed as input tokens.
- **Effort**: `output_config.effort`, set explicitly per review type (section 11), and lowered when
  the allowance is low. `max_tokens` is set per effort level as the hard cap, and includes thinking:
  a review cut off by it is `incomplete`. Forced tool choice is not available on Opus 5.5.
- **Expected cost per review**:

  | Review | Cost |
  |---|---|
  | Routine check | $0.15–0.40 |
  | Daily review | $0.8–2 |
  | xhigh plan | $2–4 |

  - A normal day is about $3–6.
  - A busy day (6 checks, the daily review, two severe xhigh reviews) is about $12.40, paid from
    rollover or by reducing reviews. The report shows how often.
- **Allowance** (this installation's agent spending, not the whole Anthropic account):
  - about $6.45 per day from $200 per month, with rollover within the month and a hard stop at $200;
  - 25% held back for daily, trigger and severe reviews;
  - effort steps down when the allowance is low;
  - at zero, reviews stop, trading continues, and an alert is sent.
- **Pre-call reservation** (an upper bound):
  - input at the dearest rate it could be billed (the 1-hour cache-write rate for tokens marked for
    caching);
  - output: `max_tokens` × the output price;
  - search: the remaining searches × ($0.01 + 25k tokens × the input price);
  - reconciled from `usage` after the call. A call with an unknown outcome keeps its reservation
    until it is reconciled, or until the month ends.
- **Break-even display (other users)**: the budget screen shows the monthly budget as a % of the
  account per year, and the extra return the agent must add over the rules manager to pay for
  itself. Example: $200/month on $15k is 16%/yr.
- **Other users** must enter a monthly budget before the agent starts.

## 14. Approvals and alerts

- **Desktop**: tray notifications for:
  - a plan waiting;
  - a partial, superseded or failed plan;
  - a safety exit or lock;
  - a breach;
  - a cap hit;
  - budget at 80% or exhausted;
  - an API key error;
  - a protection gap.
- **Telegram**:
  - **Setup**:
    - the user creates a bot and pastes its token (stored like the other keys);
    - linking happens in a **private chat** with a one-time code of at least 8 random characters,
      valid for 5 minutes, single use;
    - the **chat ID and the user ID** are stored, and the linked account's name is shown in the app.
  - **Callbacks**: accepted only from that private chat and that user.
  - **Buttons** carry an opaque token mapped to the stored plan, with no amounts or destinations.
  - **The message** is rendered from validated plan fields; the model's prose follows, marked as its
    explanation.
  - **Approvals**:
    - bound to `plan_hash`, idempotent, re-validated and gated;
    - voided by agent off, unlinking, or a relevant limit change;
    - a tap on a voided plan is answered "expired".
  - **Transport**: polling.

## 15. Keys and distribution

- **Storage**: the API key and the bot token are stored like the private key (DPAPI on Windows,
  Keychain on macOS).
- **API key use**: it is sent only in `x-api-key` to api.anthropic.com, never logged, never in a
  URL. There is a test-key button.
- **Rate limits**: honour `retry-after`. A spend-limit 429 (`enforced_spend_limit_reached`) stops
  reviews and alerts.
- **Terms**: **confirm the bring-your-own-key question with Anthropic before release.**
- **Liquidity Lens key**: optional. Until issuance exists (request R1) the owner's install uses
  a key the owner creates; later it is issued from the app after a wallet sign-in (section 6a). A
  test button shows the remaining daily quota and tells apart clock skew, quota, an unauthorised
  key and an outage.
- **Onboarding**: get a key; set a budget (with the break-even display); review the approved tokens
  and limits; set up alerts; pass the readiness checks before auto mode. It states that protection
  stops while the app is not running.

## 16. Build plan

| Phase | Scope | Size |
|---|---|---|
| 1. Accounting and data | Marked equity and cash-flow-adjusted performance per pool; **per-pool execution-cost attribution**; position geometry; data-quality flags; regime features; reference prices and token health; gas; swap-cost scenarios; **local cache for every external source (download on first use, then top up; requests and hit rate measured)**; **45-day local store for managed and candidate pools, filled from the Solidly UI data processor, with download size measured**; funding/OI snapshots; **Liquidity Lens client (`llens.rs`) with cache and staleness flags, controller and review caches, depth on the dashboard**. On the dashboard | 3–4 weeks |
| 2. Run lock, write gate, caps, risk controller | **Per-chain run lock around every unit of work, book updates inside it**; `gate.rs` before every signature; engine caps for all strategies; locks; risk controller on a wall-clock timer with heartbeat; **same-nonce replacement for stuck transactions on the emergency path**; safe-asset health selection; exposure model with projected checks and precedence; approved-token list | 3 weeks |
| 3. Plans and coordinator | Plan object and states; submission dedup; two-phase capital moves with bounds passed to both executors; re-validation; reservations; idempotent steps; pre-emption; retry-then-park; strategy-state reset on apply and revert; execution settings; journal with a hash chain; manual moves onto it | 2 weeks |
| 4. Agent | Claude adapter, prompt v1 and schema, `hold`/`submit_plan`/`escalate`, bounded tools, the strategy gate (long window + slice), scheduler, budget with reservations | 2–3 weeks |
| 5. Approvals and alerts | Agent tab, desktop notifications, Telegram | 1–2 weeks |
| 6. Baseline and shadow harness | Rules manager; shadow books **with quote-based costs**; scored report; evidence counts | 1–2 weeks |
| 7. Development shadow → freeze → scored test → small live stage | — | 6+ weeks elapsed |
| 8. MCP server | Same coordinator, queue, lock and gate | deferred: not part of this build |

**Required failure tests** (scripted, each with its expected end state):
- an emergency exit while a pool run holds the run lock: no tokens booked to the wrong pool, and the
  emergency runs next;
- a safety event between the release and deposit phases of a move;
- a stuck pending transaction during an emergency: replaced at the same nonce within the bound;
- the risk controller's heartbeat stale: every writer denied;
- a strategy switch while the pool is exited: state reset, and the new strategy decides on that
  cycle;
- `revert_strategy` to an inadmissible strategy;
- a duplicated `submit_plan` after a restart;
- a duplicated Telegram tap, and a tap from another user or a group chat;
- the first-choice safe asset depegged with a cheap route into it;
- an emergency breaching a limit, then a plan that would worsen it;
- an all-USDC start deployed (issuer count excludes the safe asset);
- an ETH drop of 6% with the cbETH/WETH strategy matching holding: the loss limit does not fire;
- an API timeout with unknown spend; the budget exhausted mid-review;
- two plans competing for the same capital;
- an engine cap hit by a frequently rebalancing graph;
- a restart mid-plan (between the two phases of a move);
- the app closed for 6 hours, with the gap recorded;
- Liquidity Lens down, stale or out of quota mid-review: data marked stale, the review completes on
  own data, and no risk condition fires or clears because of it;
- a stable moving on CEX while the pool and Liquidity Lens have not followed: the cross-class
  disagreement freezes; Liquidity Lens does not outvote the CEX price;
- USDC off peg on CEX: Liquidity Lens dropped from the checks on that chain, its USDC-quoted prices
  not used;
- a suspect source not re-confirmed: it expires after 1 hour; with no healthy off-chain price, the
  token's pools are frozen for new deployments;
- a local clock 30 s slow: keyed Liquidity Lens calls still succeed (deadline from the server's
  `Date` header);
- Liquidity Lens out of quota or down for hours: back-off, no request loop, quota not drained;
- a restart with a warm cache: no history downloaded again, only top-ups;
- a damaged cache file: that piece alone downloaded again, nothing read from it.

## 17. Still to confirm

- The cbETH/ETH exchange-rate source on Base.
- A source of market-wide liquidations, or drop the field.
- The bring-your-own-key question with Anthropic.
- The exact web-search tool version at build time.
- How Claude Desktop loads a local stdio MCP server (phase 8, deferred).
- Liquidity Lens: the priorities of the request list (`LLENS_REQUESTS.md`), above all key issuance
  (R1) and exposing the traffic mix (R5); the organisation cap and per-key caps.
