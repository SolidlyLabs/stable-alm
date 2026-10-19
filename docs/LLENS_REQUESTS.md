# Liquidity Lens: feature requests from Stable ALM

Status: draft for prioritisation, 19 Oct 2026, checked against Liquidity Lens's code by Fable. These are what the Stable ALM agent
(`AGENT_PLAN.md`, section 6a) would use from Liquidity Lens. Each request says:
- what exists in Liquidity Lens today;
- what Stable ALM needs on top;
- where the agent plan uses it;
- how we would know it is done.

The suggested priority is Stable ALM's view only; the Liquidity Lens roadmap decides.

**History limit**: Liquidity Lens keeps 2 weeks of history and will not keep more (its disk is the
limit). No request here asks for longer retention. History of Solidly pools beyond 2 weeks comes
from the Solidly UI data processor (`solidly.com/api/history`, already used by Stable ALM).

**Caching**: Stable ALM caches everything locally. It downloads a series once on first use, then
asks only for what is newer, and keeps closed buckets on its own disk beyond the 2 weeks. So
history endpoints should support a `from` cursor or timestamp: a top-up then returns only new
rows.

**Priorities**
- **P0**: needed before Stable ALM can use Liquidity Lens at all.
- **P1**: large gain for the agent.
- **P2**: useful.
- **P3**: later.

## Summary

| # | Request | Exists today | Suggested | Plan section |
|---|---|---|---|---|
| R1 | Keys issued to Stable ALM users after a wallet sign-in | Org keys, per-key limits, metering; no key-creation route, issuance is operator-only | P0 | 6a, 15 |
| R2 | A Stable ALM entitlement: the keyed oracle, read-only | Entitlements per org | P0 | 6a |
| R3 | Batch depth for a review: several tokens × sizes × both sides in one call | Oracle batch of quotes | P1 | 5, 6 |
| R4 | Hourly per-pool facts for other DEXes' pools, last 14 days (volume, fees, TVL, in-range liquidity, effective fee) | M3 `market-stats` planned | P2 | 6 |
| R5 | Traffic mix per pool through an API key, last 14 days | Volume attribution live but admin-only (whitelisted wallets; keys cannot reach it); hours kept 14 days | P1 | 6 |
| R6 | Competing pools for a pair across DEXes, with in-range liquidity near the price | `pair-competition` planned | P1 | 6, 7 |
| R7 | A token-health feed: spot vs executable gap for a size, with a flag when it widens | Spot/executable history and `/ext/oracle/reference` status live | P2 | 4 |
| R8 | Ranked ranges for a pool (`lp-research`) | M5 planned | P2 | 9 |
| R9 | The value of added depth per pool (depth-threshold curve) | Proposed | P2 | 5, 13 |
| R10 | Candidate pairs with demand | "Demand without our pools" live (admin page); `pair-discovery` planned | P3 | 7 |
| R11 | ~~Raw swap history export~~ withdrawn: Solidly pools' history comes from the data processor | — | — | 6 |
| R12 | Push alerts on a watched token or pool | Alert webhook exists for operators | P3 | 4, 11 |
| R13 | Meter a call only after a replica is available | The gateway meters first, so an outage uses quota | P2 | 6a |
| R14 | A USD price for the chain's quote token (USDC on Base), or a USD-denominated variant | The quote token is never listed; every price is in it | P2 | 4 |
| R15 | Tolerate client clock skew on `x-compute-deadline` (a relative budget, or the skew allowed stated) | Absolute epoch in ms vs the server's clock | P3 | 6a |
| R16 | Mark the oracle as live in `SYSTEM-CAPABILITIES.md` (section 25 still says planned) | Code is live | P3 | — |

## Details

### R1. Keys for Stable ALM users (P0)
- **Today**: API keys belong to an organisation; per-key limits (requests/s, burst, requests and
  quotes per day) are set by an operator (`ModOracleKeyLimit`, audited). Self-serve issuance for
  wallet users is part of M4 ("API keys with scopes").
- **Needed**:
  - a user signs in with their wallet (SIWE) from the Stable ALM app and gets a key in the Stable
    ALM organisation;
  - default per-key limits are applied at issue;
  - the user can revoke the key and see their usage;
  - the key is shown once and can be rotated.
- **Note**: per-key limits only narrow the organisation's shared cap. Issuance should keep the sum
  of per-key caps below the organisation cap, or reserve a share per key.
- **Done when**: a fresh install obtains a key without an operator, a second key for the same
  wallet replaces the first, and a revoked key is refused within a minute.
- **Until then**: the owner creates one key for their own install; other users run without
  Liquidity Lens.

### R2. Stable ALM entitlement (P0)
- **Today**: entitlements are per organisation (`OrgEntitlement`).
- **Needed**: a "stable-alm" plan with these properties:
  - access to the keyed oracle (quote, price, reference, depth, batch, history), read-only;
  - volume attribution reads (`series`, `pools`) once R5 makes them reachable by key;
  - no access to admin-only data (simulated fees, overrides, scope decisions, discovery rankings,
    wallet addresses);
  - an org-wide daily cap the owner sets.
- **Done when**: a Stable ALM key reads those endpoints, and is refused on every other one.

### R3. Batch depth per review (P1)
- **Use**: exit capacity (section 5) and the swap-cost scenarios (section 6) need, for each held
  token, the cost at 1× and 2× the amount, in both directions, all fresh at once.
- **Needed**:
  - one call returning a depth ladder for up to ~10 tokens × 4 sizes × 2 sides;
  - the block and time each answer was computed at;
  - metered as one request with N quotes.
- **Done when**: one review's market data is a single call under 2 s.

### R4. Hourly facts for other DEXes' pools, 14 days (P2)
- **Use**: comparing a Solidly pool with the other pools of its pair (fees, volume, in-range
  liquidity). Solidly pools' own history, at any age, comes from the Solidly UI data processor, so
  this is only for pools on other DEXes, which Stable ALM has no other source for.
- **Needed**: M3's hourly per-pool rows for the last 14 days, for the pools a key asks for: swaps,
  volume and fees per token in raw units, TVL, in-range liquidity, effective fee.
- **Done when**: a Stable ALM key reads 14 days of hourly rows for a Base pool on another DEX, and
  for a Solidly pool the rows match the data processor's.

### R5. Traffic mix through a key, 14 days (P1)
- **Use**: the bot share beside F/LVR (section 6) and its trend.
- **Today**:
  - the module is admin-only (`visibility: { admin: true, public: false }`);
  - its reads need a whitelisted admin wallet, so an API key or an ordinary wallet gets 403;
  - hourly per-pool data is kept 14 days; DEX-hour rollups 400 days.
- **Needed**:
  - per-pool aggregator/bot/organic share (not addresses, which stay admin-only), readable with an
    entitled key;
  - the 14 days kept today are enough; the agent uses the latest week and its trend.

### R6. Competing pools for a pair (P1)
- **Use**: the agent's "is there a better pool for this pair" question, and competing liquidity
  near the price (section 6, tier 2). Today it is built from Stable ALM's own mint/burn events, for
  the pools it syncs only.
- **Needed**:
  - for a token pair, every pool across DEXes on the chain: fee model, TVL, in-range liquidity
    within ±0.1/±0.5/±1% of the price, and 7-day fees;
  - `pair-competition` as planned, plus the near-price liquidity.

### R7. Token-health feed (P2)
- **Use**: the risk controller's token-health warning (section 4).
- **Needed**:
  - per token and size: the current gap between spot and executable price, its 24 h baseline, and
    a flag when the gap exceeds N× the baseline;
  - computed at head (like `reference`), not from the published rows the public endpoints serve;
  - one cheap read for several tokens that the controller can poll every 5 minutes, metered as
    one request.

### R8. Ranked ranges (`lp-research`, P2)
- **Use**: a second opinion beside Stable ALM's own backtests in the strategy gate.
- **Needed**: M5 as planned; the input pins a block, and the output states the fee model and the
  costs assumed.

### R9. Value of added depth (P2)
- **Use**: sizing each pool's capital: what the next $X earns in a pool, given the trade-size
  distribution.
- **Needed**: the depth-threshold curve per pool, as a few points (capital → expected share of
  volume and fees).

### R10. Candidate pairs (P3)
- **Use**: proposing new pools (`add_pool`).
- **Needed**: `pair-discovery`'s ranked candidates, limited to public data (routed volume with no
  deep pool). This is not admin rankings.

### R11. Raw swap export (withdrawn)
- The backtester's 45 days of Solidly pool events come from the Solidly UI data processor's
  public history, which Stable ALM already reads (`history.rs`). Nothing is needed from Liquidity
  Lens.

### R12. Push alerts (P3)
- **Use**: faster trigger reviews (section 11) and token-health warnings without polling.
- **Needed**: a per-key webhook or stream for a watched token's health flag and a watched pool's
  traffic jump.

### R13. Meter after the replica check (P2)
- **Today**: `oracleGateway` meters the call, then selects a replica. When no replica is
  available, the call fails but still counts against the key's and the organisation's quota.
- **Needed**: meter only calls that reached a replica, or refund failed ones. Stable ALM backs off
  either way, but an outage should not drain a day's quota.

### R14. A price for the quote token (P2)
- **Today**: prices are in the chain's canonical stable (USDC on Base), which is never listed as an
  asset. If USDC depegs, every price moves together, and Liquidity Lens says nothing about USDC
  itself. USDC is also Stable ALM's first safe asset, so this is the case its risk controller
  cares most about.
- **Needed**: a USD price for the quote token, from its pools against other stables and a stated
  reference, or prices also quoted in a second stable (USDT).

### R15. Clock skew on the deadline (P3)
- **Today**: `x-compute-deadline` is an absolute epoch in milliseconds compared with the server's
  `Date.now()`. A client whose clock is a few seconds slow fails every call with
  `ORACLE_CALLER_DEADLINE_EXPIRED`. Stable ALM works around it by using the server's `Date`
  header.
- **Needed**: accept a relative budget (e.g. `x-compute-budget-ms`), or state the skew tolerated.

### R16. Product doc (P3)
- `SYSTEM-CAPABILITIES.md` section 25 still calls the execution-aware oracle "planned", while the
  keyed and public endpoints are in the code.

## Open questions for the Liquidity Lens owner
- Is key issuance to wallet users (R1) still planned with M4, or should Stable ALM's need be
  built separately?
- Are quota costs per quote or per request for batches (R3)?
