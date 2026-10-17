// Context help sidebar: explains whatever the pointer is over (or what was tapped / focused).
// Elements carry data-help="<key>"; dynamic tables are tagged from their column headers, settings
// and strategy inputs from their keys. The panel keeps the last explanation until something else
// with help is hovered, and shows the element's live details (its tooltip text and table row).
(function () {
  const H = {
    _default: ["How to use this panel", `
      <p>Move the pointer over anything on the page (a number, a column header, a button, a setting) and this panel explains what it is,
      how it is calculated and what changing it does. On a phone, tap the item.</p>
      <p><b>What this app does.</b> It manages your concentrated liquidity on Solidly V3 automatically, on every chain Solidly runs on (Ethereum, Optimism, Base, Arbitrum, Sonic). Every cycle (5 minutes by default) the worker:</p>
      <ol><li>refreshes pool data,</li><li>values everything it manages in USD,</li><li>claims rewards when due,</li>
      <li>asks each pool's strategy whether the position should change,</li><li>runs safety checks,</li><li>and only then withdraws, swaps through the Solidly UI aggregator and re-deposits.</li></ol>
      <p>Nothing is signed until a private key is set <i>and</i> you press <b>Go live</b>. Until then every cycle is a <b>dry run</b>: the same plan, with every transaction simulated against the real chain.</p>`],

    // ---- header ----
    mode: ["Mode", `
      <p><b>DRY RUN</b>: the worker plans and simulates everything but signs nothing. This is the default and is safe to leave running to watch what it would do.</p>
      <p><b>LIVE</b>: a private key is set, <b>Go live</b> was pressed, the key's address matches the managed wallet and the pause switch is off. Transactions are really sent.</p>
      <p><b>PAUSED</b>: the kill switch is on (<code>config/STOP</code>). No action is taken in either mode. Data and statistics keep updating.</p>`],
    cycle: ["Worker cycle", `
      <p>The worker runs one cycle every <i>Cycle interval</i> seconds (Settings), or immediately after <b>Run now</b> or after saving pools.</p>
      <p><b>last cycle</b>: when the previous cycle finished, and how long it took. The first cycle on a fresh install is slower because it downloads 14 days of pool events.</p>
      <p><b>next in</b>: the countdown to the next scheduled cycle. A red message is the last cycle-level error. Pool-level errors appear on each pool's card and under Activity.</p>`],
    run_now: ["Run now", `<p>Starts a cycle immediately instead of waiting for the next scheduled one. It does not force a rebalance: each strategy still decides for itself.
      To force the ladder to re-solve, use <b>Re-solve</b> on the pool.</p>`],
    claim_now: ["Claim now", `
      <p>Claims rewards in the next cycle, ignoring the weekly cadence. The claim still follows Solidly's rules:</p>
      <ul><li>It waits while a new Merkle root needs an L3 sync, or while the ~1 hour security buffer after a root push runs.</li>
      <li>Fees in pool tokens (USDC, USDT, …) land in the wallet and are redeployed at the pool's next rebalance.</li>
      <li>SOLID is sold at that rebalance if the price is acceptable (Settings).</li></ul>
      <p>In dry run the claim is only simulated.</p>`],
    pause: ["Pause / Resume", `<p>The kill switch. While paused, no withdraw, swap, deposit or claim is sent, in live or dry mode. Pools are still valued and statistics keep updating.</p>
      <p>It is stored as the file <code>config/STOP</code>, so it survives restarts. Resume removes it and starts a cycle.</p>`],
    live: ["Go live / Switch to dry run", `
      <p><b>Go live</b> lets the worker sign and send real transactions with the stored private key from the next cycle on. It needs a key (Settings), and the key's address must equal the managed wallet.</p>
      <p><b>Switch to dry run</b> turns signing off again. Positions stay exactly as they are; nothing is withdrawn.</p>
      <p>Before going live: check that the wallet has ETH for gas (Overview), the allocations add up to what you want, and the last dry runs look right (Activity, pool cycle logs).</p>`],
    "tab-overview": ["Overview tab", `<p>Live state of everything the worker manages: total value, profit and its parts across all chains, one card per chain with its balance, gas and profit, the value history, and one card per active pool with its positions and last decision.</p>`],
    "tab-pools": ["Pools tab", `<p>One table for every pool: the ones you manage (highlighted, at the top by default) and every other Solidly V3 pool on the chains you turned on. Click a column header to sort.</p>
      <ul><li>Turn chains on or off with the chips at the top.</li><li>Search, show only your pools, or show volatile pairs (after a confirmation).</li>
      <li>Set each managed pool's allocation, enable it, and tune its strategy; then <b>Save</b> in the bar that appears.</li><li><b>Add</b> any listed pool; it starts disabled at 0%.</li></ul>`],
    "tab-settings": ["Settings tab", `<p>Global options (cycle interval, claims, reward selling, retries, the funding tokens for cross-pool allocations) and the private key.</p>`],
    "tab-activity": ["Activity tab", `<p>Every decision and transaction the worker made (newest first), and the tail of the warnings/errors log.</p>`],

    // ---- overview ----
    "k-total": ["Managed value", `
      <p>The USD value of everything the worker manages:</p>
      <ul><li>all positions in configured pools, valued at the pool price,</li><li>plus the wallet balances of those pools' tokens and the funding tokens.</li></ul>
      <p>Token prices come from the Solidly aggregator. The line below shows the change since the first recorded snapshot. It <i>includes</i> deposits and withdrawals you made yourself, so it is not pure profit.</p>
      <p>Allocations are percentages of this number.</p>`],
    "k-deployed": ["Deployed in pools", `<p>Value currently inside liquidity positions, and its share of the managed value.</p>
      <p>Close to 100% is normal. A noticeably lower share means tokens are waiting in the wallet:</p>
      <ul><li>after a failed or partial deposit (redeployed automatically next cycle),</li><li>after a claim,</li><li>or because part of the value is unallocated.</li></ul>`],
    "k-idle": ["Idle in wallet", `<p>Free balances of the managed tokens (pool tokens and the funding tokens) sitting in the wallet, not earning.</p>
      <p>When a pool's idle share exceeds <i>Redeploy idle above</i> (Settings, default 2% of its allocation), the next cycle tops the position up without withdrawing it. Smaller amounts wait for the next rebalance.</p>`],
    "k-profit": ["Profit", `<p>What the app has made for you across all chains, from its own transactions:</p>
      <p><code>profit = earned − gas paid − swap costs</code></p>
      <ul><li><b>Earned</b>: everything the reward claims paid into the wallet, plus what is claimable now. On Solidly V3 all LP income (swap fees, SOLID emissions, bribes) is paid through claims.</li>
      <li><b>Gas paid</b>: every transaction the app sent (claims, swaps, approvals, deposits, withdrawals), including the L1 data fee on OP-stack chains.</li>
      <li><b>Swap costs</b>: value given away by rebalancing swaps and SOLID sales (what went out minus what came in).</li></ul>
      <p>Each transaction is valued at the token prices of its time (today's when no snapshot is near). Later price moves of the tokens you hold are not counted here; they show in the managed value. Transactions you sent yourself are not included.</p>`],
    "k-earned": ["Earned", `<p>Claimed rewards (valued at the prices when they were claimed) plus rewards claimable now, summed over all chains. The line below splits it into claimed and claimable.</p>`],
    chaincard: ["Chain", `<p>Everything the app manages on one chain. The same wallet address is used on every chain.</p><ul>
      <li><b>Managed</b>: positions plus idle managed tokens on this chain. Allocations on this chain are percentages of this number.</li>
      <li><b>Deployed / Idle</b>: in positions vs free in the wallet.</li>
      <li><b>Gas balance</b>: the chain's native token (ETH, or S on Sonic). Red, with a warning, when it is below the minimum: the worker then takes no action on this chain until you top it up.</li>
      <li><b>Earned, gas paid, swap costs, profit</b>: as in the profit figure at the top, for this chain only.</li></ul>`],
    chainfilter: ["Chains", `<p>Which chains' pools are listed and scanned (APR windows, fee / arb loss). Turning a chain on downloads its pool list; turning it off hides its other pools.</p>
      <p>Pools you already manage keep running on any chain, on or off. Fantom is not offered: Solidly retired it.</p>`],
    chain: ["Chain", `<p>The chain the pool is on. Each chain has its own balances, gas and allocation: a pool's % is of what is managed on its own chain.</p>`],
    search: ["Search", `<p>Filters the table by pair, token symbol, pool name, chain name or pool address.</p>`],
    onlymine: ["My pools", `<p>Shows only the pools in your configuration, on every chain.</p>`],
    newchain: ["Chain for the address", `<p>The chain the pasted pool address is on.</p>`],
    "k-rewards": ["Claimable rewards", `<p>Rewards you can claim now from Solidly's Rewards Distributor, valued in USD: trading fees in pool tokens, SOLID emissions, and bribes.</p>
      <p>Rewards are computed off-chain and published as a Merkle root about every 24 h. After each root push there is a ~1 hour security buffer before claiming opens. The line below shows the auto-claim cadence, or why claiming is waiting right now.</p>`],
    "k-gas": ["ETH for gas", `<p>Native ETH in the wallet. Every withdraw, swap, approval, deposit and claim costs gas, typically a few cents on Base.</p>
      <p>Below 0.0002 ETH the worker refuses to act, so it never gets stuck halfway through a rebalance. Top up with a little ETH (0.002–0.005 lasts a long time).</p>`],
    chart: ["Value history", `<p>Recorded after every cycle (<code>logs/snapshots.jsonl</code>): the managed value (positions + idle managed tokens) <b>plus unclaimed rewards and the SOLID in the wallet</b>, so earnings show as they accrue instead of as a jump at each claim. Hover a point for the breakdown.</p>
      <p>Allocations still use the managed value only. Points recorded before 16 Oct 2026 23:06 have no rewards data and show the managed value alone.</p>
      <p>Jumps can come from your own deposits and withdrawals, claims and token price moves, as well as from LP performance. Small dips right after a rebalance are swap costs and the price difference paid to move ranges.</p>`],
    poolcard: ["Pool card", `<p>One active pool.</p><ul>
      <li><b>Deployed</b>: value in its positions now.</li>
      <li><b>Target</b>: its allocation % of the managed value.</li>
      <li><b>Tick · fee</b>: the pool's current price tick and current dynamic fee (pips, 100 pips = 1 bp).</li>
      <li>The position list shows each range [lower tick, upper tick) with a green dot when the current price is inside it (earning fees) and red when outside.</li>
      <li><b>Last decision</b>: what the strategy decided in the last cycle. Open <i>cycle log</i> for the full step-by-step plan.</li></ul>`],
    position: ["Position", `<p>A liquidity range [lower tick, upper tick). Tick t is the price 1.0001^t (token1 per token0); on a pegged pair 1 tick ≈ 0.01%.</p>
      <p>A position only earns fees while the price is inside its range (green dot). The ladder strategy uses 1-tick-wide positions: very capital-efficient while the peg holds, re-solved every few hours as the price moves.</p>`],
    decision: ["Last decision", `<p>What the pool's strategy decided in the last cycle:</p><ul>
      <li><b>keep</b>: nothing to do.</li>
      <li><b>rebalance</b>: withdraw everything and redeploy into new ranges (the ladder re-solved and more than its turnover threshold moved, or the pool held more than 110% of its budget).</li>
      <li><b>topup</b>: add idle tokens to the current ranges without withdrawing.</li>
      <li><b>exit</b>: allocation 0% or pool disabled, so all positions are withdrawn.</li>
      <li><b>guard</b>: the depeg guard froze the pool because the price is far from its 7-day median.</li>
      <li><b>error</b>: the pass failed. Details are in the log and Activity, and it is retried next cycle.</li></ul>`],
    wallet: ["Wallet (managed tokens)", `<p>Free balances of the tokens the worker manages: every configured pool's two tokens plus the funding tokens.</p>
      <p>Other tokens in the wallet are ignored and never touched, except the reward tokens listed for selling (SOLID).</p>`],
    rewards: ["Rewards", `<p>Claimable amounts per token from Solidly's rewards API (<code>/api/rewards/&lt;wallet&gt;</code>), valued in USD.</p>
      <p>The claim submits the API's Merkle claim object unchanged to <code>rewarder.claimAll</code> after simulating it.</p>
      <p>Compounding: claimed pool tokens join the next rebalance. SOLID is sold into the pool's numeraire during that rebalance, in the largest chunk that loses at most the configured %. SOLID liquidity is thin, so small amounts are kept until they are worth selling.</p>`],

    // ---- pools ----
    allocation: ["Allocation of the managed value", `<p>Allocation is per chain: each enabled pool targets its percentage of the value managed <i>on its own chain</i>. Example: with $15,000 managed on Base, 60% → $9,000 target. Each chain's pools may add up to at most 100%.</p>
      <ul><li>Unallocated percent stays idle in the wallet.</li>
      <li>Raising a pool's % makes the next cycle top it up from idle tokens. If the pool needs tokens the wallet does not hold, they are bought with the funding tokens through the aggregator.</li>
      <li>Lowering a pool's % below what it holds (by more than 10%) triggers a rebalance down to the new target.</li>
      <li>0% or disabled withdraws the pool completely.</li></ul>
      <p>Changes apply after <b>Save</b>, on the cycle that starts right away.</p>`],
    blend: ["Blended APR", `<p>The estimated APR of the <i>whole</i> managed value with the current allocation: each pool's estimated APR at its target size, weighted by its target. Idle percent counts as 0%.</p>
      <p>Shown for the last 1 h, 6 h and 24 h, with the yearly dollars at the 24 h rate. These are raw processor figures: the backtests assume 82% of fees are actually paid and SOLID is worth half, so real results tend to be lower. Short windows are noisy, because volume is lumpy.</p>`],
    "p-color": ["Colour", `<p>The pool's colour in the allocation bar.</p>`],
    "p-pool": ["Pool", `<p>The pool's name in this app, its token pair and its contract on the chain's explorer.</p>`],
    "p-enabled": ["Enabled", `<p>Disabled pools get no new deposits. If a disabled pool still holds positions, the next cycle withdraws them. Use it to exit a pair without deleting its settings.</p>`],
    "p-alloc": ["Allocation %", `<p>The pool's share of the value managed on its chain. Type it, then <b>Save pools &amp; allocation</b>. Enabled pools on one chain may add up to at most 100%. The switch enables or disables the pool; The ⚙ button opens its strategy parameters, Re-solve and Remove; the line below shows what it holds now.</p>`],
    "p-target": ["Target", `<p>Allocation % × managed value: the dollars this pool should hold. The worker tops up when the pool holds clearly less (idle tokens above the redeploy threshold) and rebalances down when it holds more than 110% of it.</p>`],
    "p-deployed": ["Deployed", `<p>The value of this pool's positions right now.</p>`],
    apr: ["Estimated APR (if deposited)", `<p>What a deposit would have earned over the window, annualised:</p>
      <p><code>(fees + emissions + bribes) × 8760 / hours ÷ (in-range TVL + deposit)</code></p>
      <ul><li><b>Inputs</b>: fees, emissions and bribes are what the whole pool paid its in-range LPs, from the Solidly data processor's hourly history.</li>
      <li><b>Deposit</b>: your deposit joins the in-range liquidity and shares pro rata, like the average in-range LP. In <i>Pools</i> it is the pool's target size, with your own existing liquidity taken out of the TVL so it is not counted twice. A pool at 0% shows a 10% slice. For other pools it is the deposit size you entered.</li>
      <li><b>Windows</b>: 1 h reacts fast but is noisy; 24 h is steadier. Large differences mean volume came in bursts.</li>
      <li><b>Caveats</b>: these are raw figures. The backtests count fees at 82% (what the rewarder actually paid) and SOLID at 50%. The ladder's 1-tick positions usually earn more than the average in-range LP, and wide ranges earn less.</li></ul>
      <p>Hover a value for its breakdown, shown under "This value" below.</p>`],
    "p-strategy": ["Strategy & limits", `<p>The strategy that decides where the liquidity goes, plus this pool's safety limits. Open it with the ⚙ button on the pool's row; changes apply after <b>Save</b>.</p>
      <p><b>ladder</b> (recommended, best in walk-forward backtests on every pair): every <i>every</i> bars it looks at the last <i>lookback</i> seconds and finds which ticks paid fees and how crowded each was with other LPs. It then spreads the capital over single ticks to maximise its fee share, using √(fees × competing liquidity) − competing liquidity. It only re-deposits if more than <i>turnover</i> of the capital would move.</p>`],
    "param-lookback": ["lookback (seconds)", `<p>How much recent history the ladder uses to decide where fees are paid. 86400 = 1 day, which was best in backtests: it follows the peg's slow drift but ignores single bursts. Longer is steadier; shorter reacts faster but churns more.</p>`],
    "param-span": ["span (ticks)", `<p>How far from the median tick (of the lookback window) the ladder may place liquidity: ±span ticks. 3 for USDC/USDT (±0.03%); 6 for cbETH/WETH, which moves more. Wider costs nothing if those ticks pay no fees, because the solver gives them nothing.</p>`],
    "param-every": ["every (5-minute bars)", `<p>The re-solve cadence: 72 bars = 6 hours. The ladder waits this long between re-solves, which also gives the price time to come back: backtests showed waiting beats acting immediately on every pair. Use <b>Re-solve</b> to force one.</p>`],
    "param-turnover": ["turnover (fraction)", `<p>Minimum share of capital that must move before a re-solve is executed. 0.4 = only rebalance if at least 40% of the allocation would change ticks. It avoids paying swaps and gas for small improvements.</p>`],
    "param-guard": ["guard (ticks)", `<p>The depeg guard. While the spot price is more than this many ticks from its 7-day median (15 ticks ≈ 0.15%), the pool freezes: no rebalance and no swap. It resumes when the price comes back.</p>
      <p>This avoids selling the depegged coin at the bottom. In March 2023 (USDC to $0.87) it cut the simulated ladder loss from −$785 to −$16 on $15k.</p>`],
    "param-slippage_bps": ["slippage_bps", `<p>Tolerance for the min-amount checks:</p><ul><li>withdraw and deposit minimums are the expected amounts × (1 − slippage);</li>
      <li>the aggregator's minimum output uses the same figure.</li></ul><p>2 bp suits stable pairs; a transaction that would breach it reverts instead of filling badly.</p>`],
    "param-max_swap_cost_bps": ["max_swap_cost_bps", `<p>The most a rebalancing swap may cost, measured against the pool's own price and including all fees. If the aggregator's best quote is worse, the swap is skipped: what can be deposited is deposited, and the rest waits for a better quote next cycle. 3 bp for stables. Typical quotes are about 0 bp or even better than the pool.</p>`],
    "param-max_spot_dev_ticks": ["max_spot_dev_ticks", `<p>Manipulation and fast-move check: no action while the current tick is further than this from the last hour's median tick. It protects against depositing around a price that was just pushed by a large trade.</p>`],
    "param-max_actions_per_day": ["max_actions_per_day", `<p>The most rebalances, top-ups or exits per UTC day for this pool. One rebalance is several transactions. It caps runaway behaviour if something goes wrong.</p>`],
    "param-min_swap_value": ["min_swap_value", `<p>The smallest imbalance (in token0 units) worth a swap. Smaller differences are deposited as they are or left idle. 5 for USD stables, 0.002 for ETH pairs.</p>`],
    "param-max_ranges": ["max_ranges", `<p>The most positions this pool may hold. If the strategy wants more (a volatile pair's ladder can ask for dozens of tick ranges), neighbouring ranges are merged into this many contiguous groups of similar weight.</p>
      <p>Each position costs one deposit and one withdrawal transaction per rebalance, so fewer positions mean cheaper, faster rebalances. Stable pairs normally use 1–4 anyway.</p>`],
    resolve: ["Re-solve", `<p>Makes this pool's ladder re-solve on the cycle that starts now, instead of waiting for its 6-hour cadence. It only rebalances if the new solution moves more than <i>turnover</i>.</p>`],
    remove: ["Remove", `<p>Deletes the pool from the configuration. Only possible once it holds no positions: set it to 0% (or disable it) first and let a cycle withdraw it.</p>`],
    savepools: ["Save pools & allocation", `<p>Writes enabled flags, allocations and strategy settings to <code>config/pools.json</code> and starts a cycle. Enabled allocations above 100% are rejected.</p>`],
    discard: ["Discard changes", `<p>Throws away unsaved edits in this table and reloads the saved configuration.</p>`],

    // ---- discover ----
    depsize: ["Deposit size for the estimate", `<p>The dollar amount the APR columns assume you deposit. A bigger deposit takes a bigger share of the same fees, so its APR is lower; small pools are hit hardest. Defaults to your managed value.</p>`],
    mintvl: ["Min TVL", `<p>Hides pools with less total value locked than this. Tiny pools can show huge APRs that disappear with any deposit. Configured pools are always shown.</p>`],
    onlysupported: ["Only pegged pairs", `<p>Shows only pairs whose two tokens track the same asset: both USD stablecoins, ETH and its liquid-staking tokens (cbETH, wstETH, weETH…), BTC wrappers, EUR or MXN stables. Their prices must also agree within 0.8–1.25. A token that merely trades near $1 (e.g. AERO) does not count. The ladder strategy and its safety settings were backtested on these.</p>
      <p>On by default. Turning it off asks you to confirm first, because the strategy has not been tested on volatile pairs.</p>
      <p>Volatile pairs (e.g. WETH/USDC) can be added and run, but nothing has been backtested for them: concentrated liquidity on a volatile pair can lose more to price moves (divergence loss) than it earns in fees.</p>`],
    pair: ["Pair", `<p>The pool's tokens, its address on the chain's explorer and its tick spacing. Spacing is the tick granularity of positions: 1 for pegged pairs, larger for volatile pairs. <i>whitelisted</i> means Solidly emissions can be voted to the pool.</p>`],
    fee: ["Fee", `<p>The pool's fee tier (bp = 0.01%) and the current dynamic fee. Solidly V3 adjusts fees per pool; the current fee is what swaps pay right now and what LPs earn per unit of volume.</p>`],
    tvl: ["TVL", `<p>The total value locked in the pool, in all ranges, in USD (Solidly UI data).</p>`],
    activetvl: ["In-range TVL", `<p>The value of the liquidity whose range contains the current price, i.e. the liquidity that is actually earning fees now. Your deposit shares fees with this, not with the full TVL.</p>`],
    vol24: ["24h volume", `<p>Swap volume over the last 24 hours (USD). Fees ≈ volume × fee. Volume is lumpy: one aggregator route change can double or halve it.</p>`],
    poolapr: ["Pool APR 24h", `<p>The data processor's own 24 h APR (fees + emissions) on the in-range liquidity, before your deposit. Compare it with the estimated columns to see how much your deposit would dilute the yield.</p>`],
    flvr: ["Fee / arb loss (7d)", `<p>Whether liquidity at the current price earns more in fees than it loses to arbitrage. Over the last 7 days, per unit of in-range liquidity:</p>
      <ul><li><b>F</b>: the fees one unit would have earned (each swap's fee divided by the in-range liquidity it was shared with),</li>
      <li><b>LVR</b> (loss versus rebalancing): what arbitrageurs take from that unit as the price moves, from the squared hourly price returns.</li></ul>
      <p>The figure is F ÷ LVR, before swap and gas costs. Above 1, fees pay for the arbitrage loss; below 1, LPing at the money loses money whatever the strategy.</p>
      <p><b>How to read it:</b></p>
      <ul><li><b>Above 100: good.</b> Fees dwarf the arbitrage loss; the volume is real trading. Our pegged and LST pools normally sit here (USDC/USDT in the millions, cbETH/WETH from about 10 to tens of thousands, varying week to week).</li>
      <li><b>10 to 100: fine.</b> Fees still clearly cover the loss. For cbETH/WETH this is a normal quiet week.</li>
      <li><b>2 to 10: watch it.</b> Still profitable, but thin. Check whether volume dropped or the price started moving.</li>
      <li><b>Below 2: bad (shown as a warning).</b> The flow is mostly arbitrage, as in a depeg. Consider shrinking the position. The worker does not act on this by itself; the depeg guard is what freezes a pool.</li>
      <li><b>Below 1: losing.</b> Liquidity at the money loses more to arbitrage than it earns, whatever the strategy. Deep volatile pools sit near 1; Solidly WETH/USDC sits at about 0.55, which is why it is not worth LPing.</li></ul>
      <p>Hover the figure for F, LVR and the number of swaps used. It is shown for every pool. Configured pools use their stored swaps and hourly fee history; other pools use the last 7 days of swaps downloaded for the whole chain, priced at the pool's current fee. It shows "–" until the first download has finished, a minute or two after the app starts.</p>`],
    addpair: ["Add", `<p>Adds the pool to the configuration, <b>disabled at 0%</b>, with the default ladder strategy. Its tokens, decimals and tick spacing are read on-chain, and its data downloads on the next cycle.</p>
      <p>Give it an allocation in the same table. Ideally backtest it first; the strategy parameters were tuned on USDC/USDT and cbETH/WETH.</p>`],
    configured: ["Configured", `<p>This pool is in your configuration (the name shown). Its row is highlighted and has the allocation controls.</p>`],
    volatile: ["Volatile · not backtested", `<p>The two tokens do not track the same asset (they are not both USD stables, ETH variants, BTC wrappers, …). Any decimal combination works (e.g. WETH 18 / USDC 6). Prices and liquidity are converted per token.</p>
      <p>When added, the pair gets volatile defaults:</p>
      <ul><li>a wider ladder (span 60 tick spacings);</li><li>no depeg guard, which would freeze on every normal move;</li>
      <li>a looser 1 h spot check (100 ticks);</li><li>10 bp slippage and a 15 bp swap-cost cap.</li></ul>
      <p>None of this has been backtested. Expect divergence loss when the price trends, and keep allocations small until it proves itself.</p>`],
    addaddr: ["Add by address", `<p>Pick the chain and paste any Solidly V3 pool address to add it (disabled, 0%). It works for pools not shown in the list.</p>`],
    refresh: ["Refresh", `<p>Reloads the pool list and the APR windows. Data is cached for a few minutes: pools 5 min, APR windows 10 min.</p>`],

    // ---- settings ----
    "set-interval_s": ["Cycle interval", `<p>Seconds between worker cycles (default 300). Strategies keep their own cadence (the ladder re-solves every 6 h), so a shorter interval mostly means faster reaction to failures, idle tokens and depeg-guard releases. Very short intervals add RPC and aggregator load.</p>`],
    "set-idle_redeploy_pct": ["Redeploy idle above", `<p>When a pool's free tokens exceed this % of its allocation, the next cycle deposits them into the current ranges (a top-up) without waiting for the next rebalance. It recovers failed or partial deposits and new allocations quickly. Smaller amounts, such as weekly claims, wait for the next rebalance.</p>`],
    "set-resize_tolerance_pct": ["Resize tolerance", `<p>A rebalance changes only what has to change. A position whose range is still in the new target is kept: it is trimmed with a partial withdrawal when it holds too much, or topped up by depositing into the same range when it holds too little. Only ranges that leave the target are withdrawn in full.</p><p>Differences smaller than this % of the pool's allocation (and never less than the pool's minimum swap value) are left alone, so small drifts don't cost transactions. Default 1%.</p>`],
    "set-claim.enabled": ["Auto-claim rewards", `<p>Claims rewards automatically every <i>Claim every</i> days, following Solidly's Merkle flow:</p>
      <ol><li>Read the snapshot.</li><li>Wait for the L3 sync and the security buffer if needed.</li><li>Check the earners are this wallet.</li><li>Simulate.</li><li>Submit <code>claimAll</code>.</li></ol>
      <p>A broadcast claim is never re-sent in the same cycle.</p>`],
    "set-claim.every_days": ["Claim every (days)", `<p>How often to claim. Weekly balances gas against compounding. Rewards keep accruing safely in the distributor until claimed.</p>`],
    solidmode: ["Claimed SOLID", `<p>What happens to SOLID the worker claims:</p>
      <ul><li><b>Keep in the wallet</b>: nothing; it waits for you.</li>
      <li><b>Sell into the pool at the next rebalance</b>: sold through the aggregator into the pool's tokens and deposited with them (limited by the max sale loss and min sale value below).</li>
      <li><b>LP full range</b>: deposited into a SOLID pool as one full-range position, paired with the same USD value of the pool's other token (limits under "SOLID LP" below).</li></ul>`],
    "set-sell_rewards.enabled": ["Sell SOLID at rebalance", `<p>During a pool's rebalance, claimed SOLID is sold into the pool's numeraire through the aggregator and deposited with the rest. That is the compounding step.</p><p>Off: SOLID stays in the wallet.</p>`],
    "set-sell_rewards.max_impact_pct": ["Max SOLID sale loss", `<p>The most value a SOLID sale may lose against the aggregator's own USD valuation. If selling everything loses more, it tries half, a quarter and so on, and sells the largest chunk within the limit. SOLID liquidity on Base is thin (selling ~1,800 SOLID lost ~20% when tested).</p>`],
    "set-solid_lp.enabled": ["LP claimed SOLID, full range", `<p>Instead of selling claimed SOLID, the worker deposits it into a SOLID pool as one <b>full-range</b> position, paired with the <b>same USD value</b> of the pool's other token. Example: $40 of SOLID goes in with $40 of WETH.</p>
      <p>Each cycle, if the wallet's SOLID is worth at least the minimum:</p>
      <ul><li>it checks that the pool's SOLID price is close to the market price (the SOLID pools are thin; depositing at a stale price would give value to arbitrageurs);</li>
      <li>the pair token comes from the wallet first, and the rest is bought through the aggregator with your funding tokens, largest balance first;</li>
      <li>everything is added to the same full-range position. The worker never withdraws it.</li></ul>
      <p>This position is outside the allocation split, but the pair token it buys comes out of the managed balance. While this is on, SOLID is never sold, and every SOLID in the wallet is deposited, not only freshly claimed SOLID.</p>`],
    solidlppool: ["SOLID LP pool", `<p>The pool the SOLID goes into. Only Solidly V3 pools with SOLID are listed, with their current TVL. The default is WETH/SOLID (1% fee), the deepest one. The position is full range: it never goes out of range and earns that pool's fees and emissions on its share of liquidity. Like any 50/50 position, it is exposed to SOLID's price against the pair token (impermanent loss).</p>`],
    "set-solid_lp.min_value_usd": ["Min SOLID to deposit", `<p>SOLID in the wallet worth less than this waits for more claims, so gas is not spent on dust deposits.</p>`],
    "set-solid_lp.max_price_dev_pct": ["Max pool price deviation", `<p>The deposit waits while the pool's SOLID price (from its current tick, in USD through the pair token) differs from the aggregator's market price by more than this. Depositing into a mispriced thin pool would hand the difference to the first arbitrageur.</p>`],
    "set-solid_lp.slippage_bps": ["SOLID LP mint slippage", `<p>Minimum amounts on the full-range mint: the transaction reverts if the pool price moves so that it would take less than this many basis points below the planned amounts. 50 bp suits a 1% fee pool.</p>`],
    "set-solid_lp.max_swap_cost_bps": ["Max cost buying the pair", `<p>The most a swap that buys the pair token (for example USDC to WETH) may lose against the aggregator's USD valuation, fees included. A worse quote is retried, then skipped; the deposit then uses whatever pair token is there.</p>`],
    "set-sell_rewards.min_value_usd": ["Min SOLID sale value", `<p>A SOLID chunk worth less than this is kept for later, so gas is not spent on dust.</p>`],
    "set-retries.attempts": ["Retries per step", `<p>How often each step (withdraw, swap, approval, deposit) is retried after a failure, with fresh chain state and a fresh quote each time. Withdraw failures stop the pass before anything else happens. Swap and deposit failures leave tokens idle for the next cycle's top-up.</p>`],
    "set-retries.backoff_s": ["Retry backoff", `<p>Seconds to wait before a retry, multiplied by the attempt number (10 s, 20 s, …). It gives a congested RPC or a moving price time to settle.</p>`],
    "set-wallet": ["Managed wallet", `<p>The address whose positions and balances are managed. It must equal the private key's address for live trading. Saving a key switches it automatically.</p>`],
    funding: ["Funding tokens", `<p>The tokens used to buy what a pool needs when its allocation requires tokens the wallet does not hold. Example: you hold USDC and USDT and give cbETH/WETH 30%, so the worker buys cbETH/WETH with them.</p>
      <p>You can tick several. A shortfall is bought from the selected token with the largest USD balance first; if that runs out, the next one covers the rest. Shortfalls under $10 are ignored.</p>
      <p>The list shows every token of every Solidly pool with your wallet balance; ticked tokens are listed first, and the closed picker shows them as chips (× removes one). A selected token that is one of the pool's own tokens is not swapped by funding; the normal rebalancing swap handles it. <b>None</b> clears the selection and turns cross-pool funding off: each pool then only uses its own tokens.</p>`],
    privkey: ["Private key", `<p>Paste the wallet's private key to allow live trading. It is encrypted for your user account (Windows DPAPI or the macOS Keychain), so the worker resumes after restarts without asking.</p>
      <p>The API never returns it and logs never contain it; only its address is shown. Saving it switches the managed wallet to its address. <b>Remove</b> deletes the file and turns live trading off.</p>`],
    helpvis: ["Show explanations panel", `<p>Shows or hides this panel. It is on by default. The choice is remembered in this browser only (not a worker setting), and takes effect immediately without saving.</p>
      <p>With the panel hidden, hovering still works as normal tooltips where values have breakdowns.</p>`],
    savesettings: ["Save settings", `<p>Writes the options and the funding tokens to <code>config/settings.json</code>. They take effect on the next cycle.</p>`],
    safety: ["Safety", `<p>Checks before any action:</p><ul><li>kill switch,</li><li>depeg guard,</li><li>spot vs 1 h median,</li><li>enough gas on the chain,</li><li>daily action cap.</li></ul>
      <p>Every transaction is simulated with <code>eth_call</code> and <code>estimateGas</code> before it is sent. Swaps only go through the Solidly UI meta-aggregator, capped against the pool price, with exact-amount approvals. Stuck transactions are re-broadcast with higher fees.</p>`],

    // ---- activity ----
    events: ["Activity", `<p>Every decision and transaction, newest first (<code>logs/executor.jsonl</code>). Hover an event type for its meaning. Transaction hashes link to the chain's explorer. "(dry)" marks dry-run rows.</p>`],
    "ev-sent": ["sent", `<p>A real transaction was broadcast and mined successfully. The link opens it on the chain's explorer.</p>`],
    "ev-dry_run": ["dry_run", `<p>A transaction that would have been sent in live mode. It was built and, where its inputs already exist on-chain, simulated successfully.</p>`],
    "ev-solid_lp": ["solid_lp", `<p>The start of a SOLID deposit into the SOLID pool (full range, paired with the same $ of the other token). The amount of SOLID and its USD value are in details.</p>`],
    "ev-rebalance": ["rebalance", `<p>The start of a rebalance, top-up or exit (the mode is shown in details). Live ones count towards the daily cap.</p>`],
    "ev-keep": ["keep", `<p>The strategy saw no reason to change anything this cycle.</p>`],
    "ev-retry": ["retry", `<p>A step failed and will be retried with fresh state. Common causes: an aggregator hiccup (502), a price move past the min amounts, or a stale quote.</p>`],
    "ev-error": ["error", `<p>A step failed after all retries, or a whole pool pass failed. Tokens stay where the last confirmed transaction left them; idle tokens are redeployed by the next cycle's top-up. Details are in the errors log below.</p>`],
    "ev-blocked": ["blocked", `<p>A safety check stopped an action: kill switch, gas, price far from the 1 h median, or the daily cap. Nothing was sent.</p>`],
    "ev-guard_freeze": ["guard_freeze", `<p>The depeg guard froze the pool because the price is more than <i>guard</i> ticks from its 7-day median. It waits for the price to return.</p>`],
    errors: ["Errors & warnings", `<p>The tail of <code>logs/errors.log</code> (newest first): warnings such as retries and skipped swaps, and errors with tracebacks. If an alert webhook or Telegram is configured, errors are also sent there, at most once an hour per message.</p>`],
  };

  const TH = {"APR 1h": "apr", "APR 6h": "apr", "APR 24h": "apr", "Pool APR 24h": "poolapr", "TVL": "tvl", "In-range TVL": "activetvl", "24h volume": "vol24", "Fee / arb 7d": "flvr",
    "Fee": "fee", "Pair": "pair", "Chain": "chain", "Pool": "p-pool", "Enabled": "p-enabled", "Allocation": "p-alloc", "Target": "p-target", "Deployed": "p-deployed",
    "Strategy & limits": "p-strategy", "Token": "wallet", "Claimable": "rewards", "When": "events", "Event": "events", "Details": "events"};
  const BTN = {"Run now": "run_now", "Claim now": "claim_now", "Pause": "pause", "Resume": "pause", "Go live": "live", "Switch to dry run": "live",
    "Re-solve": "resolve", "Remove": "remove", "Add": "addpair", "Save pools & allocation": "savepools", "Discard changes": "discard", "Save settings": "savesettings",
    "Save key": "privkey", "Refresh": "refresh", "Add (disabled, 0%)": "addaddr"};

  function tag(root) {
    const set = (el, k) => { if (el && !el.dataset.help) el.dataset.help = k; };
    root.querySelectorAll("#tabs button").forEach(b => set(b, "tab-" + b.dataset.t));
    root.querySelectorAll("button").forEach(b => { const k = BTN[b.textContent.trim()]; if (k) set(b, k); });
    root.querySelectorAll("table").forEach(t => {
      const heads = [...t.querySelectorAll("tr:first-child th")].map(th => TH[(th.dataset.label ?? th.textContent).trim()] || null);
      t.querySelectorAll("tr:first-child th").forEach((th, i) => heads[i] && set(th, heads[i]));
      t.querySelectorAll("tr").forEach(tr => [...tr.children].forEach((td, i) => { if (td.tagName === "TD" && heads[i]) set(td, heads[i]); }));
    });
    root.querySelectorAll("#settings [data-k]").forEach(el => set(el.closest("div"), el.dataset.k === "solid_mode" ? "solidmode" : "set-" + el.dataset.k));
    root.querySelectorAll("#pools .fields > div").forEach(d => { const l = d.querySelector("label"); if (l) set(d, "param-" + l.textContent.trim()); });
    root.querySelectorAll("#poolCards .card").forEach(c => set(c, "poolcard"));
    root.querySelectorAll("#poolCards .pos-list > div").forEach(d => set(d, "position"));
    root.querySelectorAll("#poolCards .card > .small").forEach(d => set(d, "decision"));
    root.querySelectorAll("#events tr").forEach(tr => { const c = tr.children[3]; if (c && c.tagName === "TD") set(c, "ev-" + c.textContent.replace(/\(dry\)/, "").trim()); });
    root.querySelectorAll(".chip.volatile").forEach(c => set(c, "volatile"));
    root.querySelectorAll("#pools .chip.mine").forEach(c => set(c, "configured"));
    root.querySelectorAll("#allocs .card").forEach(c => set(c, "allocation"));
    root.querySelectorAll("#chainBar .chipbtn").forEach(c => set(c, "chainfilter"));
    const ids = {mode: "mode", cycle: "cycle", chart: "chart", tokens: "wallet", rewards: "rewards", chainBar: "chainfilter", q: "search", newChain: "newchain", depSize: "depsize", minTvl: "mintvl", fundingPicker: "funding", pk: "privkey", keyStatus: "privkey", errors: "errors", newPool: "addaddr", newName: "addaddr"};
    for (const [id, k] of Object.entries(ids)) set(document.getElementById(id), k);
    const kp = {kTotal: "k-total", kProfit: "k-profit", kEarned: "k-earned", kDeployed: "k-deployed", kIdle: "k-idle"};
    for (const [id, k] of Object.entries(kp)) { const e = document.getElementById(id); if (e) set(e.closest(".card"), k); }
    set(document.getElementById("solidModeBox"), "solidmode");
    set(document.getElementById("solidLpBox"), "solidlppool");
    const hv = document.getElementById("helpVisible"); if (hv) set(hv.closest("label"), "helpvis");
    const os = document.getElementById("onlySupported"); if (os) set(os.closest("label"), "onlysupported");
    const om = document.getElementById("onlyMine"); if (om) set(om.closest("label"), "onlymine");
    set(document.getElementById("allocSum"), "allocation");
    document.querySelectorAll("section[data-tab=settings] .card h2").forEach(h => { if (h.textContent.trim() === "Safety") set(h.closest(".card"), "safety"); });
  }

  // ---- panel ----
  const css = document.createElement("style");
  css.textContent = `
    #help{position:fixed;z-index:4;background:var(--card);border-left:1px solid var(--line);overflow:auto;font-size:13.5px;line-height:1.55}
    #help h3{margin:0 0 8px;font-size:15px}#help p,#help ul,#help ol{margin:0 0 9px}#help ul,#help ol{padding-left:18px}#help li{margin:2px 0}
    #help code{background:var(--card2);border:1px solid var(--line);border-radius:5px;padding:0 4px;font-size:12px}
    #help .kicker{font-size:11px;letter-spacing:.6px;text-transform:uppercase;color:var(--mute);margin-bottom:6px;display:flex;align-items:center;gap:8px}
    #help .live{margin-top:10px;padding:9px 10px;border:1px solid var(--line);border-radius:9px;background:var(--card2);font-size:12.5px}
    #help .live b{display:block;font-size:11px;letter-spacing:.5px;text-transform:uppercase;color:var(--mute);margin-bottom:3px}
    #helpToggle{display:none}
    body.nohelp #help{display:none!important}
    @media (min-width:1100px){body.nohelp{padding-right:0!important}}
    @media (max-width:1099px){body.nohelp{padding-bottom:0!important}}
    @media (min-width:1100px){body{padding-right:380px}#help{top:0;right:0;bottom:0;width:380px;padding:18px 18px 40px}}
    @media (max-width:1099px){#help{left:0;right:0;bottom:0;max-height:38vh;border-left:0;border-top:1px solid var(--line);padding:12px 16px 18px;box-shadow:0 -8px 24px rgba(0,0,0,.3)}
      body{padding-bottom:40vh}#helpToggle{display:inline-block}#help.min{max-height:44px;overflow:hidden}body.helpmin{padding-bottom:60px}}`;
  document.head.appendChild(css);
  const panel = document.createElement("aside");
  panel.id = "help";
  document.body.appendChild(panel);
  let current = null;

  // Visibility preference (per browser; default visible). Settings → Interface, or "hide" in the panel.
  function stored() { try { return localStorage.getItem("alm.help") !== "off"; } catch (e) { return true; } }
  function setVisible(on) {
    document.body.classList.toggle("nohelp", !on);
    try { localStorage.setItem("alm.help", on ? "on" : "off"); } catch (e) {}
    const cb = document.getElementById("helpVisible");
    if (cb) cb.checked = on;
  }
  window.almHelp = {setVisible, visible: () => !document.body.classList.contains("nohelp")};

  function render(key, el) {
    const h = H[key] || H._default;
    current = key;
    let live = "";
    if (el) {
      const tip = el.getAttribute("title") || (el.querySelector && el.querySelector("[title]") && el.querySelector("[title]").getAttribute("title"));
      const tr = el.closest && el.closest("tr");
      const rowName = tr && tr.parentElement && tr !== tr.parentElement.firstElementChild ? (tr.children[0].innerText || tr.children[1]?.innerText || "").trim().split("\n")[0] : "";
      const val = el.tagName === "TD" ? el.innerText.trim() : "";
      if (rowName || val || tip) live = `<div class="live"><b>This value</b>${rowName ? esc(rowName) + (val ? ": " : "") : ""}${val && val.length < 80 ? "<span>" + esc(val) + "</span>" : ""}${tip ? "<div class=mute>" + esc(tip) + "</div>" : ""}</div>`;
    }
    panel.innerHTML = `<div class="kicker"><span>Explain</span><span style="flex:1"></span><button id="helpToggle" class="sec sm" type="button">${panel.classList.contains("min") ? "show" : "collapse"}</button>
      <button id="helpHide" class="sec sm" type="button" title="hide this panel (turn it back on in Settings → Interface)">hide</button></div>
      <h3>${h[0]}</h3>${h[1]}${live}`;
    panel.querySelector("#helpToggle").onclick = () => { panel.classList.toggle("min"); document.body.classList.toggle("helpmin"); render(current, null); };
    panel.querySelector("#helpHide").onclick = () => setVisible(false);
  }
  function esc(s) { return String(s ?? "").replace(/[&<>"']/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"}[c])); }
  function pick(e) {
    const el = e.target.closest && e.target.closest("[data-help]");
    if (!el || panel.contains(e.target)) return;
    const td = e.target.closest("td,th");
    render(el.dataset.help, td && td.dataset.help === el.dataset.help ? td : el);
  }
  document.addEventListener("mouseover", pick);
  document.addEventListener("focusin", pick);
  document.addEventListener("click", pick, true);
  let t = null;
  new MutationObserver(() => { clearTimeout(t); t = setTimeout(() => tag(document), 60); }).observe(document.body, {childList: true, subtree: true});
  tag(document);
  render("_default", null);
  setVisible(stored());
})();
