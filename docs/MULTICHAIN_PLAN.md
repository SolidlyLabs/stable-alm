# Plan: chain-agnostic ALM (Base today; Ethereum, Arbitrum, Optimism, Sonic, Fantom next)

Goal: no chain is hard-coded in the runtime.
- **Every pool entry names its chain.**
- **All chain-specific values come from one registry:** RPC, block timing, gas token, stablecoin,
  SOLID, explorer, and Solidly API chain ids.
- **The dashboard shows positions on every chain** and can be filtered by chain.

This is being built into the Rust port from the start (`rs/`). The Python code stays Base-only and
is the reference for parity.

## 1. What was Base-specific in the original Python executor

| Where | Base assumption | Multichain replacement |
|---|---|---|
| `rpc.py` | `/api/rpc/8453/`, `BASE_GENESIS_TS + 2*block` for timestamps | per-chain read/send RPC; timestamps from block headers (see §3) |
| `settings.py` | `USDC`, `SOLID`, `SOLID_WETH_POOL` constants | registry fields per chain |
| `datasync.py` (removed) | `43_200` blocks/day, 1M-block segments | blocks/day from the registry (or measured); segments by block number per chain |
| `engine.py` | gas check `< 0.0002 ETH`, chainId 8453 in signing | per-chain gas-token minimum; chainId from the registry |
| `claim.py` (removed) | `rewards/{wallet}/?chainId=8453` | `chainId` of the pool's chain; one claim per chain |
| `aggregator.py` / `prices.py` | `chainId=8453`; USD via USDC on Base | chain's id and its USD stable |
| `market_data.py` | `getInitData` for chain 8453 | one call per enabled chain, merged and tagged |
| `worker.py` (removed) | one account total, one funding pass | per-chain totals and funding (see §4) |
| UI | Base explorer links, one pool list | chain column, chain filter, per-chain explorer and icons |

## 2. Chain registry (`config/chains.json`, shipped defaults + user overrides)

```json
{
  "8453": {"name": "Base", "enabled": true, "rpc": "https://solidly.com/api/rpc/8453/",
           "send_rpc": "https://mainnet.base.org", "explorer": "https://basescan.org/",
           "block_time_s": 2, "genesis_ts": 1686789347, "gas_symbol": "ETH", "min_gas": "0.0002",
           "usd": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
           "solid": "0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd",
           "solid_lp_pool": "0xd09f2ebaf365fc97a5bbdefc15703312165638e4"},
  "42161": {"name": "Arbitrum", "enabled": false, "rpc": "https://solidly.com/api/rpc/42161/", ...},
  "10": {"name": "Optimism", ...}, "1": {"name": "Ethereum", ...}, "146": {"name": "Sonic", ...},
  "250": {"name": "Fantom", ...}
}
```

- **Which chains:** the ones the Solidly UI supports (`stores/constants/constants.js`): 1, 10, 250,
  8453, 42161, 146.
- **Defaults, Base:** filled from what runs today.
- **Defaults, other chains:** RPC, explorer and name come from solidly-ui. Token addresses (`usd`,
  `solid`) are taken from each chain's `getInitData` token list by symbol on first use, and cached
  in the registry. The user can override any field.
- **No `genesis_ts` or `block_time_s`:** timestamps come from headers instead (see §3).
- **Pool entries gain `"chain_id"`.** A missing value means 8453, so existing configs keep working.
- **Pool identity:** `chain_id:address`, not the address alone. The same address can exist on two
  chains.

## 3. Runtime changes

- **Every chain call takes a chain.** rpc and chain become `Rpc { chain }` handles, and pinned reads
  are tracked per chain. The pool's chain flows through the executor, claims, prices and the
  aggregator.
- **Timestamps:** Base keeps its exact formula. Other chains read the timestamps of a sparse set of
  block headers (every ~1,000 blocks, batched) and interpolate. The ladder only needs seconds to
  ~1 minute accuracy over 24 h, so this is enough.
- **Blocks per day:** measured from two headers a day apart at startup (cached), instead of 43,200.
  Used for the bootstrap window and the 10-day live window.
- **Data folders:** `data/raw/<chain_id>/<pool>/…`. On first run, existing Base data moves from
  `data/raw/<pool>/`.
- **Logs:**
  - Executor log entries gain `chain_id`.
  - `minted_ranges` and `actions_today` filter by chain + pool.
  - Transaction hashes link to that chain's explorer.
- **Gas:** the "low gas" check uses the chain's own gas token and minimum (S on Sonic, FTM on Fantom,
  ETH elsewhere).
- **Signing:** the chain id comes from the registry. The same key and address work on every EVM
  chain. Nonces are per chain, so pools on different chains can run one after another without
  conflict.
- **Claims:** one rewards snapshot, root sync and claimAll per chain with configured pools. The
  rewarder address already comes from the API response. Claim state goes in
  `logs/last_claim_<chain>.json`.
- **Prices:** USD via the chain's own stablecoin, through the aggregator with that chain id.
- **SOLID handling** (sell, or LP into the chain's SOLID pool) is per chain.

## 4. Allocation across chains

Tokens can't move between chains without bridging, and the ALM won't bridge: bridging carries its
own risk and delay. So:
- **The account total is per chain:** pool positions + free tokens on that chain.
- **A pool's `allocation_pct` is a share of its own chain's total.** The UI checks each chain's
  enabled pools stay ≤ 100%.
- **Funding tokens are per chain:** a list in the registry, defaulting to the chain's USD stable.
  Funding swaps stay on-chain.
- **The UI also shows a grand total across chains,** but nothing is ever allocated from it.

## 5. UI

- **Overview:**
  - one row per position across all chains: chain icon and name, pair, range, amounts, USD, in
    range or not;
  - totals per chain plus a grand total.
- **Chain filter:** chips "All · Base · Arbitrum · …", showing only chains that have pools or a
  balance. It applies to positions, pools, events, value history (a per-chain line breakdown) and
  Discover. The choice is remembered per browser.
- **Pools & allocation:**
  - grouped by chain, with a chain picker when adding a pool;
  - allocation sums are checked per chain.
- **Discover:** merges `getInitData` from every enabled chain, with a chain column and the same
  filter.
- **Settings:**
  - a "Chains" section: enable or disable a chain, RPC URL overrides, gas minimum, funding tokens;
  - claim and SOLID options per chain.
- **Explorer links and gas balances** are per chain (header: ETH 0.01 · S 3.2 …).
- **Status API:**
  - `worker.pools` is keyed by `chain_id:name`;
  - each entry carries `chain_id`;
  - account snapshots carry `chains: {id: {total_usd, pools, tokens, gas}}`.

## 6. Order of work

1. **Registry + chain parameter through rpc/chain/prices/aggregator** (Rust port: built in now).
   Base-only behaviour must stay identical; the parity tests prove it.
2. **Per-chain data layout and header-based timestamps**, plus the Base data migration.
3. **Per-chain account totals, allocation checks, funding and claims.**
4. **UI:** chain field, filter chips, grouped pools, per-chain explorer and gas.
5. **Enable a second chain in dry run only**, verify plans and simulations, then enable live per
   chain.

## 7. Risks

- **Non-Base chains are untested for this strategy:** depth, fees and emissions differ. A pool there
  needs its own backtest before it gets an allocation.
- **RPC load:** the Solidly read proxy has a 300 requests/min per IP limit, per chain route.
  Several chains in one 5-minute cycle stay well under it, but the data bootstrap on a new chain
  must be paced.
- **Rewards API:** must support each `chainId`, which the UI already relies on. Its absence for a
  chain just disables claims there.
