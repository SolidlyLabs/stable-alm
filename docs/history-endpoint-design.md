# Pool history from the data processor: design

Status: built (2026-10-18). The processor serves these routes and solidly.com proxies them at `/api/history/`; the app reads them in `rs/src/history.rs`. Goal: ALM users get pool history (for live decisions, the
profit ledger, Discover's fee-vs-arb figure and custom-mode backtests) from Solidly's data
processor through a keyless, rate-limited public endpoint, instead of each user pulling it with
`eth_getLogs` from free RPCs (about 40 requests a minute per chain, 10,000 blocks per request).

## What the processor already stores (SolidlyLabs/v3-data-processor, prisma/schema.prisma)

| ALM needs | Stored as | Exposed today |
|---|---|---|
| Swap events: block, log index, time, tick, sqrtPriceX96, active liquidity, amounts | `PoolState`, one row per pool event (`event` = SWAP / MINT / BURN / FLASH / …): `blockNumber`, `blockTimestamp`, `globIndex`, `tick`, `previousTick`, `sqrtPriceX96`, `activeLiquidity`, `swap0DeltaWei`, `swap1DeltaWei`, `txHash`, `sender`, `recipient`, `origin` | no; only bucketed in `/pool/tick-history` |
| Mint / Burn with owner, range and liquidity | `PositionState` (`liquidityDelta`, `owner`, `poolStateId`, `globIndex`) joined to `Position` (`owner`, `tickLower`, `tickUpper`) | no; current positions only (`/position/earnings`) |
| Hourly volume, fees, emissions | `PoolHourlySnapshot`, apr history | yes (`/pool/apr-history`, `/pool/hourly-snapshots`) |
| Pool list: tokens, fee, tick spacing | `Pool` (`token0Id`, `token1Id`, `initialFee`, `tickSpacing`), `Token` | partly; fee and spacing only via the internal watch API |
| USD prices | `TokenPrice`, `PoolState.token*PriceUSD` | yes (`/pool/prices`) |
| A wallet's own pool transactions | `PositionState.owner` + `txHash` | no |
| Fee collects of a position | not stored (only `COLLECT_PROTOCOL_FEES`) | — |

- **Order and cursor.** `globIndex = blockNumber × 10^10 + logIndex` (`src/lib/utils.ts`). It is a
  strict event order, and `logIndex = globIndex mod 10^10`.
- **Retention.** `data-purge-service.ts` deletes rows only after a reorg or a requested resync.
  History is kept from each pool's creation.
- **Indexes the queries need already exist.** `PoolState (blockchain, poolId, event, globIndex)`,
  `(blockchain, poolId, globIndex desc)` and `(blockchain, globIndex, poolId)`;
  `PositionState (blockchain, poolStateId)`.
- **Freshness.** The processor syncs from the subgraph. `SyncState.lastGlobIndex` and
  `lastSubgraphHeadBlockNumber` say how far it is, and the watch finality service knows what is
  final.

So nearly all of the RPC history load can move to the processor. The remaining RPC work is the
last few minutes past the processor's head, and transaction receipts for gas and aggregator swaps
(the ledger).

## Endpoints

### 1. Processor (internal, existing bearer token, same tRPC/OpenAPI style as `/pool/tick-history`)

`GET /api/rest/pool/events/{blockchain}/{poolId}?fromTs=&toTs=&kinds=SWAP,MINT,BURN`

```json
{
  "pool": "0x…", "fromTs": 1792195200, "toTs": 1792281600,
  "headGlobIndex": "…", "finalGlobIndex": "…", "complete": true,
  "columns": ["g", "ts", "k", "tick", "sqrtP", "liq", "a0", "a1", "owner", "tl", "tu", "dl", "tx"],
  "rows": [["368123450000000012", 1792195231, "S", 17, "7923…", "1622…", "-5000000", "4999820", null, null, null, null, "0x…"]]
}
```

- Rows are arrays, not objects (about 3× smaller), and amounts are strings in wei.
- `k` is S / M / B. Mint and Burn rows carry `owner`, `tl`, `tu` and `dl` (liquidity delta) from
  `PositionState ⨝ Position`.
- One query on `PoolState (blockchain, poolId, event, globIndex)`, with a left join on
  `PositionState` for M/B rows.
- At most one UTC day per call, which also caps the response size.

`GET /api/rest/chain/swaps/{blockchain}?fromTs=&toTs=`: every pool's swaps for one day (for
Discover's 7-day fee-vs-arb). Same rows plus a `pool` column, without the M/B fields. It uses
`(blockchain, globIndex, poolId)` with `event = SWAP`.

`GET /api/rest/pool/meta/{blockchain}`: the public subset of the pool metadata (pool, token0/1 with
decimals and symbol, fee, tickSpacing, createdBlock).

`GET /api/rest/position/events/{blockchain}/{owner}?afterGlobIndex=`: a wallet's Mint/Burn rows
with `txHash`. The ledger then needs receipts only for those transactions, not a log scan.

### 2. Public proxy in solidly-ui (`pages/api/alm/v1/…`, keyless)

```
GET /api/alm/v1/{chain}/pools
GET /api/alm/v1/{chain}/pool/{pool}/events/{YYYY-MM-DD}
GET /api/alm/v1/{chain}/swaps/{YYYY-MM-DD}
GET /api/alm/v1/{chain}/wallet/{owner}/events?after={globIndex}
GET /api/alm/v1/{chain}/head          → { headGlobIndex, finalGlobIndex, headBlock, headTs }
```

- **Day buckets, not free cursors.** The URL names one pool and one UTC day, so every client asks
  for the same keys and the responses cache well.
  - A day that has ended and is below `finalGlobIndex` never changes. It is served with
    `Cache-Control: public, max-age=31536000, immutable` and stored in Redis, so each bucket costs
    the database one query, ever.
  - The current day gets `max-age=60`.
  - A bucket the processor hasn't finished (`complete: false`) gets `no-store`.
- **Limits.** These are per IP and kept in Redis, so they hold across instances. The existing
  `lib/apiRateLimit.ts` keeps its counts in memory; it needs a Redis store behind the same interface.
  - 120 requests a minute on cached buckets, which cost almost nothing.
  - 30 a minute on uncached ones (today's bucket, wallet events).
  - A global cap of concurrent processor queries, for example 8; above it the proxy answers 503
    with `Retry-After`.
  - Answers are 429 with `Retry-After`, which the ALM client already honours.
- **Validation.** Supported chain, lowercase 40-hex pool or owner, and a day between the pool's
  creation and today. Only pools that exist in `Pool` are served, so nobody can make the proxy
  scan for arbitrary addresses.
- **Payload.** gzip/br (one active pool-day is a few hundred KB raw, about 15% of that compressed).
- **Abuse surface.** The data is all public chain data, so the risk is cost, not secrecy. Immutable
  caching plus the limits cap the cost. The processor token stays server-side, as for the routes
  solidly-ui proxies today.

### 3. ALM client (StableALM)

- **Local cache.** History goes in `data/history/<chain>/<pool>/<day>.json.gz`. Finished, final
  days are written once and never fetched again. A first sync of 90 days is 90 requests, about
  90 s at 1 a second.
- **Tail.** After the processor's `headBlock`, the client reads the rest with `eth_getLogs`
  (minutes of blocks, usually one request). Live decisions keep their current freshness.
- **Fallback.** If the endpoint is down, rate limited for long, or returns `complete: false`, the
  client uses the existing RPC download for that range.
- **Spot check on first use per pool.** The client compares one recent hour from the endpoint with
  `eth_getLogs`: event count, last tick, and amount signs. If they disagree, it falls back to RPC
  and logs the mismatch.
- **Settings.** One setting, "History source: Solidly API (default) / RPC only", for users who
  don't want to depend on the service.

## To check before building

1. **Amount sign convention.** Does `swap0DeltaWei` follow the Swap event (positive means into the
   pool)? Check with one known swap against its receipt.
2. **Active liquidity.** Is `activeLiquidity` the value after the swap, which is what the Swap event
   reports and what the replay uses?
3. **Lag and reorgs.** How far the processor runs behind head on each chain (`/head` will show
   it), and that reorg purges don't touch rows below `finalGlobIndex`, which the immutable cache
   relies on.
4. **Query cost.** Time the chain-wide swaps query on Base for a busy day, and confirm it uses
   `(blockchain, globIndex, poolId)` rather than scanning.

## Work, in order

1. Processor PR: the four routes, zod schemas, OpenAPI entries, and tests on a seeded Postgres,
   following the `tick-history` route.
2. solidly-ui PR: `/api/alm/v1` proxy, the Redis-backed limiter, bucket caching and headers, and
   tests for validation, limits and cache headers.
3. StableALM: history source, local day cache, RPC tail and fallback, and the spot check. The
   custom-mode backtester then reads the same cache.
4. Deploying the processor and solidly-ui changes (Coolify) needs your go-ahead.
