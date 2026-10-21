# Hedge venues: Hyperliquid vs SYMM (Symmio)

Status: research, 20 Oct 2026. Nothing is built. Read from each project's live docs on that date,
plus Hyperliquid's public API (`metaAndAssetCtxs`, `fundingHistory`).

**Why:** a perp short would hedge the price exposure of a volatile pool (e.g. WETH/USDC), so the
pool earns its fees without the directional risk. Today's pools don't need it:
- USDC/USDT is pegged;
- cbETH/WETH tracks the cbETH/ETH ratio, and no venue lists a cbETH perp.

**Recommendation: Hyperliquid.** Build it only when Stable ALM runs a volatile pool.

## Side by side

| | Hyperliquid | SYMM (Symmio) |
|---|---|---|
| **What it is** | Its own L1 (HyperCore) with an on-chain order book for perps. HyperEVM is its EVM chain. | A clearing layer: traders send quotes, and whitelisted solvers take the other side bilaterally. Solvers hedge elsewhere. |
| **Who uses it** | Most on-chain perp traders and bots. Pear's main venue, its vaults, and the HLP vault. | Frontends only, not traders directly. On Base: Based, Befi, BMX, Carbon, Cloverfield, Peppy, Pear (its Base app), Privex, QuickSwap, Treble, Vibe, Xpanse. Elsewhere: Thena (BSC), SpookySwap (Sonic), Vibe (HyperEVM). |
| **ETH market (20 Oct)** | $646M/day volume, 1.08M ETH open interest, up to 25× leverage | One solver's quote. No public book depth. |
| **Trading fees** | 0.045% taker / 0.015% maker (tier 0), lower with volume or staked HYPE | 3 bps settlement fee, plus the solver's spread and fee, plus the frontend's close fee (set per affiliate) |
| **Funding** | Hourly. 1/8 of an 8-hour rate, capped at 4%/h. Paid on the oracle price. | Set by the solver per symbol, applied every 4-hour epoch (example config). No cap documented. |
| **ETH funding, last 30 days** | Shorts earned +8.85% APR on average; positive 92% of hours | Not public |
| **Counterparty** | The exchange, with transparent liquidations | The solver, backed by collateral locked per trade. A limit close it won't fill can be force-closed against a Muon oracle price, after cooldowns. A market close can't be. |
| **Getting USDC in** | CCTP (native USDC, the preferred route) or the legacy Arbitrum bridge (minimum 5 USDC, credited in under a minute). A direct route from Base was not confirmed in the docs. | Deposit on the chain you trade on. Base works for frontends, but see the bot row. |
| **Getting USDC out** | `withdraw3`, signed by the account key; arrives on Arbitrum in about 3–4 minutes (about $1 fee) | 12-hour cooldown, or a paid express provider |
| **Bots** | REST and WebSocket API. Official Rust SDK (`hyperliquid_rust_sdk`, MIT). API ("agent") wallets sign trades for the account. | The bot SDK (`@symmio/trading-core`, TypeScript) supports **only HyperEVM** (solver: Enigma). Other chains need the manual guide: MultiAccount/sub-account, session keys, Muon signatures. |
| **Rate limits** | 1200 weight/min per IP. Actions: 1 per USDC traded, plus a starting buffer of 10,000. | Set by each solver's API |

**Why Hyperliquid wins:**
- It is the deeper and cheaper market, with funding that is published and has paid shorts.
- Its API is documented down to signing.
- Withdrawals take minutes instead of 12 hours.

SYMM's one advantage, staying on Base, disappears for a bot: its bot SDK runs on HyperEVM, which is
Hyperliquid's chain. A Base integration would also make Stable ALM depend on one solver's pricing
and uptime.

## What a Hyperliquid hedge would take in Stable ALM

**Already there:**
- `agent/refdata.rs` reads Hyperliquid's `metaAndAssetCtxs` (funding and OI snapshots);
- AGENT_PLAN §13 plans funding and OI nodes, with a backfill from `fundingHistory`;
- `eth.rs` signs secp256k1 hashes (k256);
- `signer.rs` stores secrets in the OS keystore.

**New:**
1. **Hedge model in the backtester first.**
   - Per volatile pool: the net exposure of the positions plus idle tokens (the delta of a
     concentrated range is known from its ticks).
   - Target short = delta × hedge ratio, rebalanced when it drifts outside a band.
   - Costs: taker fees, funding (history from `fundingHistory`), and the margin the short ties up
     instead of earning in the pool.
   - Decision rule: hedge only if fees − (fees and funding of the short) beat the unhedged pool
     after drawdown.
2. **`hyperliquid.rs` client**, written in-house rather than with the SDK, to keep dependencies small.
   - `/info`: `clearinghouseState`, `meta`, `l2Book`, `userFunding`.
   - `/exchange`: `order` (IOC limit with a slippage cap, reduce-only for closes), `cancel`,
     `updateLeverage` (isolated, 2–3×).
   - Signing has two schemes:
     - **L1 actions:** msgpack of the action, nonce, vault and `expiresAfter` → keccak →
       EIP-712 "phantom agent" (`Exchange` domain, chain 1337).
     - **User-signed actions** (`withdraw3`, `usdSend`, `approveAgent`): EIP-712 `HyperliquidSignTransaction`.
     - The docs ask for byte-for-byte parity with the Python SDK. Pitfalls: msgpack field order, no
       trailing zeros, lowercase addresses. Needs a msgpack crate plus parity tests against SDK vectors.
   - Nonces: the current millisecond, from an atomic counter. Each signer keeps its 100 highest nonces.
3. **Keys.**
   - The wallet stays the account (its USDC is deposited).
   - A separate **API wallet** key, approved once by the wallet with `approveAgent` (valid up to
     180 days), signs every order.
   - Withdrawals and transfers are user-signed actions, so they need the wallet key; the API key
     should not be able to move funds out. Confirm this on testnet before relying on it.
   - Never reuse a deregistered API wallet address: old signed actions could be replayed.
   - The worker stays the only signer.
4. **Moving money:** Base USDC → Hyperliquid by CCTP (confirm the Base route) or through Arbitrum.
   Margin top-ups and withdrawals belong in the capital flow (`capital.rs`), with their own pending
   requests.
5. **Risk controller:**
   - distance to the liquidation price (`clearinghouseState`), with a floor that tops up margin or cuts the short;
   - a kill switch that closes the short when the pool is withdrawn or paused;
   - a funding alarm when shorts start paying;
   - venue health (stale info, rejected orders).
6. **Dashboard:** the hedge per pool, margin and liquidation distance, funding earned and paid, and
   P&L of pool and hedge together.

**Rough size:** 3–4 weeks: the backtest model 1, client and signing 1–1.5, the capital flow and
risk controller 1, the dashboard 0.5.

**Open decisions for the owner:**
- A new venue changes the rule that tokens are swapped only through the Solidly UI aggregator: the
  hedge is a perp position, not a swap. Is that allowed?
- Margin per pool and maximum leverage (2–3× isolated is suggested).
- Which volatile pool comes first.

## Sources
- Hyperliquid docs (hyperliquid.gitbook.io): exchange endpoint, signing, nonces and API wallets,
  rate limits, perpetuals info, bridge, fees, funding, margining.
- Hyperliquid API, 20 Oct 2026: `metaAndAssetCtxs`, and `fundingHistory` for ETH over 30 days.
- SYMM docs (docs.symm.io): overview, building a trading bot, perps deployments, solvers,
  settlement costs, liquidations, force closing, funding rates, withdrawals.
