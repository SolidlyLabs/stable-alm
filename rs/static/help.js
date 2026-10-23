// Context help sidebar: explains whatever the pointer is over (or what was tapped / focused).
// Elements carry data-help="<key>"; dynamic tables are tagged from their column headers, settings
// and strategy inputs from their keys. The panel keeps the last explanation until something else
// with help is hovered, and shows the element's live details (its tooltip text and table row).
(function () {
  const H = window.HELP = {
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
      <p><b>LIVE</b>: the wallet has a private key, <b>Go live</b> was pressed for it, and neither it nor the app is paused. Transactions are really sent.</p>
      <p><b>PAUSED</b>: the wallet was paused (Settings → Wallets), or the kill switch <b>Pause all</b> is on. Nothing is signed. Data and statistics keep updating.</p>
      <p><b>WATCH</b>: the wallet has no key; it is only valued.</p>
      <p>Each wallet has its own state; the header shows each one's.</p>`],
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
      <p><b>Go live</b> lets the worker sign and send real transactions with the stored private key from the next cycle on. It is per wallet, and needs that wallet's key (Settings → Wallets).</p>
      <p><b>Switch to dry run</b> turns signing off again. Positions stay exactly as they are; nothing is withdrawn.</p>
      <p>Before going live: check that the wallet has ETH for gas (Overview), each pool's capital is what you want (Pools), and the last dry runs look right (Activity, pool cycle logs).</p>`],
    "tab-overview": ["Overview tab", `<p>Live state of everything the worker manages: total value, profit and its parts across all chains, one card per chain with its balance, gas and profit, the value history, and one card per active pool with its positions and last decision.</p>`],
    "tab-pools": ["Pools tab", `<p>One table for every pool: the ones you manage (highlighted, at the top by default) and every other Solidly V3 pool on the chains you turned on. Click a column header to sort.</p>
      <ul><li>Show or hide chains' pools with the chips at the top (a filter only: your pools are managed on every chain).</li><li>Search, show only your pools, or show volatile pairs (after a confirmation).</li>
      <li>Enable each managed pool and tune its strategy, then <b>Save</b> in the bar that appears. Give it money with <b>Add</b> (from Unallocated), take it out with <b>Withdraw</b>.</li><li><b>Add</b> any listed pool; it starts disabled, with no capital.</li></ul>`],
    "tab-settings": ["Settings tab", `<p>Global options (cycle interval, claims, reward selling, retries, the private key, funding tokens and RPC endpoints per chain.</p>`],
    "tab-activity": ["Activity tab", `<p>Every decision and transaction the worker made (newest first), and the tail of the warnings/errors log.</p>`],
    "tab-lab": ["Strategy lab tab", `<p>Build strategies, see them on a pool's real price history, backtest them against strategies you pick, and save them under a name.</p>
      <ol><li>Choose a pool (yours, any listed pool, or any pool by address).</li><li>Pick a strategy with <b>📂 Select strategy</b>: built in, or one you saved (each shows its APR for the period).</li>
      <li>Change it: a node strategy by wiring blocks (indicators, conditions, outputs), simple rules in the form or by dragging handles on the chart.</li>
      <li>Every change re-runs the backtest; the cards on the right compare it with the strategies you chose.</li>
      <li><b>Save</b> it to your strategies, then pick it for a pool under <b>Pools</b>. The worker runs exactly what you tested.</li></ol>`],

    // ---- strategy lab ----
    lab: ["Strategy lab", `<p>Pick the pool to test, the period (how many days back from now) and the capital.</p>
      <p>The first run on a pool downloads its history from the Solidly data processor (compressed, a few seconds). After that it is kept on this computer and only the newest events are fetched, so re-runs take a moment.</p>`],
    "lab-pool": ["Pool", `<p>The pool to backtest on. Click to open the list: <b>your pools</b> first, with the liquidity you have in each, its share of the account, how many ranges it holds and whether they are in range (green dot) or out (amber); then <b>all pools</b> on the supported chains by TVL, with 24-hour volume and fee APR. Type to search by token, pool name or chain, filter by chain, and use the arrow keys and Enter. <b>Another pool by address</b> loads any pool.</p>`],
    "lab-period": ["Period", `<p>How far back the backtest starts. It always ends now. Choose 7 days to two years (2y); a pool younger than that runs on all its history, from its first swap, and the dates above the results say so. Every pool the run depends on is downloaded over the period too: the pools a token price block reads, and the pegged pools parking uses. The first run of a long period downloads the pool's history once (a year of a busy pool takes a few minutes and about 2 GB of memory while the replay is built, two years about twice that); later runs read it from this computer. Longer periods cover more market conditions; the most recent week says most about how the pool trades today.</p>`],
    "lab-capital": ["Capital", `<p>How much the backtest deploys, in US dollars: you bring USDC and the backtest buys the pool's tokens with it (where the pair has no stablecoin, that purchase pays the swap cost, shown under the results). The replay counts in the pair's numeraire and the cards show dollars at today's price. It starts at what you would really have there: for your pool, what is deployed in it now; for another pool, an equal share of the account.</p>
      <p><b>Fees</b> start from the fee the pool paid per unit of liquidity, and your liquidity deepens the pool. Deeper pools draw more flow (aggregators route more through them), by how much is the pool's <b>elasticity β</b>, measured on its last week and shown under the results: at β = 1 flow grows in proportion and the fee APR is the same for any amount; at 0 volume is fixed and more capital only splits it thinner. Your own positions going in and out of range are the cleanest measurement; a week of hourly fees is noisy, so the estimate is weighted against a prior of 0.5 by how sure it is. No position draws more than twice a swap's real flow. <b>Emissions do:</b> they are a fixed budget per pool, so more capital is a smaller share each. <b>Gas</b> is a fixed cost per move, heavier on a small amount.</p>`],
    "lab-library": ["Strategies", `<p>Every strategy you can open, compare with or apply: <b>built in</b> (the ladder, the ladder as a block, a Bollinger band range and two strategies that wait out moves on volatile pairs: the ones that held up in a two-year test on Base pools) and <b>saved by you</b> under your own names (kept in <code>config/strategies.json</code>).</p>
      <p>When the list opens, each strategy is backtested on the chosen pool, period and capital (four at a time, after costs), so every row shows its <b>APR</b>, what it added over holding, its moves and its time in range. Sort by APR, name or group; search by name or description.</p>
      <ul><li>Click a row to open it in the editor.</li><li><b>compare</b> adds it to the backtest comparison on the right (up to four).</li><li><b>✕</b> deletes one of your own strategies.</li></ul>
      <p>Shortcuts: <kbd>Ctrl+O</kbd> opens this list, <kbd>Ctrl+S</kbd> saves, <kbd>Esc</kbd> closes.</p>`],
    "lab-save": ["Open, new and save", `<p><b>📂 Select strategy</b> lists every strategy with its APR. <b>＋ Create new strategy</b> starts a node strategy, simple rules or a ladder. Click the name to rename.</p>
      <p><b>Save</b> (<kbd>Ctrl+S</kbd>) overwrites your own strategy, or asks for a name the first time; <b>Save as…</b> keeps a copy under a new name (built-in strategies are never changed). <b>Delete</b> removes one of yours.</p>`],
    "lab-compare": ["Comparisons", `<p>The strategies the backtest runs next to the one you are editing, on the same swaps, with the same capital and costs: up to four from the library (the ladder by default), plus holding half and half. Each gets a card and a line in the value chart.</p>
      <p><b>×</b> on a card removes it; <b>＋ Add a strategy</b> under the cards lists the library (with each strategy's APR once the strategy list has run) and holding. Your choice is remembered.</p>`],
    "lab-toolbox": ["Toolbox", `<p><b>＋ Blocks</b> opens the list of blocks (sources, indicators, maths, conditions, position and outputs): type to search, click one to add it in the middle of the view, or drag it onto the canvas. <b>Tidy</b> lays the blocks out in columns, sources on the left and outputs on the right, ordered so wires cross as little as possible; <b>−</b>, the percentage (fit everything) and <b>+</b> zoom.</p>`],
    "lab-nodes": ["Node editor", `<p>A node strategy is a small flow: <b>sources</b> (price, bar high/low, fees) feed <b>indicators</b> and <b>maths</b>, which feed <b>conditions</b>, which drive the <b>outputs</b>.</p>
      <ul><li><b>Range</b> (required) is the range to hold: its lower and upper edges, in ticks, rounded out to the tick spacing.</li>
      <li><b>Rebalance when</b> moves to that range when its condition is true (by default when the price leaves the current range), with a delay and a cooldown.</li>
      <li><b>Exit when</b> withdraws everything, <b>Hold when</b> freezes all action.</li></ul>
      <p>Add blocks with <b>＋ Blocks</b> in the toolbox or a right-click on the canvas; drag a block by its title, drag from an output dot (right) to an input dot (left) to connect, click a wire or drag it off an input to remove it. Amber outputs are true/false, green ones are prices in ticks, blue ones are other numbers. <b>Tidy blocks</b> lays them out in columns.</p>
      <p><b>Right-click</b> the empty canvas to add a block where you clicked (type to search), paste a copied block, tidy or fit. Right-click a block to name it, change its unit, reset its settings, duplicate, copy, disconnect or delete it; right-click a wire to remove it. With a block selected: <kbd>Del</kbd> deletes it, <kbd>Ctrl+D</kbd> duplicates, <kbd>Ctrl+C</kbd> / <kbd>Ctrl+V</kbd> copy and paste.</p>
      <p>Hold the middle mouse button (or drag the empty background, or one finger on a phone) to move around the canvas; the mouse wheel (or a pinch) zooms around the pointer. The minimap in the corner shows every block and the part in view: click or drag in it to jump there, right click it to see every block. <b>+</b>, <b>−</b> and the percentage (fit everything in view) sit above it.</p>
      <p>Indicator periods are in hours (bars are 5 minutes). Every value is computed from bars that have closed, so the backtest never looks ahead; the Donchian channel uses the bars before the current one, and Ichimoku has no lagging span (it would look ahead). EMA and Wilder averages start from a simple average, and on a flat price the oscillators show a neutral value instead of dividing by zero. A chain of indicators may need at most 190 hours of history before its first value. Blocks with a missing input are outlined red and the backtest waits until they are connected.</p>`],
    "lab-range": ["Range", `<p>The one range custom mode keeps around the price. In the chart it is the green bar at today's price; drag its top or bottom edge to change it.</p>`],
    "lab-width": ["Width", `<p>The range's width in ticks (1 tick is a 0.01% price step). It is rounded up to the pool's tick spacing.</p>
      <p>Narrower ranges earn a bigger share of the fees while the price is inside, but leave the range more often, and every move costs gas and a swap.</p>`],
    "lab-placement": ["Placement", `<p><b>Centred</b>: each new range is placed around the price, with the share below set by the slider.</p>
      <p><b>Follow</b>: the width stays; after the price leaves at the top the new range ends just above the price, after it leaves at the bottom it starts just below. A trend keeps the position on the price's side.</p>`],
    "lab-skew": ["Share below the price", `<p>Where the price sits inside a new range: 50% centres it; less puts more of the range above the price (expecting a rise), more puts it below.</p>`],
    "lab-offset": ["Follow offsets", `<p>With follow placement: how many ticks past the price the new range reaches after a move up (from the top) or down (from the bottom).</p>`],
    "lab-trigger": ["Trigger", `<p>When a move happens: the price must leave the trigger band (the range widened by the buffers, dashed blue on the chart), stay out for the delay, and the cooldown since the last move must be over.</p>`],
    "lab-buffer": ["Buffers", `<p>Ticks added to each side of the range before a move triggers. A buffer lets the price leave the range for a while without a move. A negative buffer moves the range before the price reaches its edge.</p>
      <p>Drag the blue trigger handles on the chart to set them.</p>`],
    "lab-delay": ["Delay", `<p>How long the price must stay outside the trigger band before the range moves. It filters out brief spikes; if the price comes back in time, nothing happens.</p>`],
    "lab-cooldown": ["Cooldown", `<p>The minimum time between two moves, so a choppy market cannot trigger a move every cycle.</p>`],
    "lab-cutoff": ["Cutoffs", `<p>Fixed prices beyond which the range is not moved: the position waits there instead of chasing the price. Useful on a pegged pair, where a price far from the peg is likely to come back.</p>
      <p>Drag the amber lines on the chart.</p>`],
    "lab-exit": ["Exit", `<p>Fixed prices that withdraw everything when crossed (after the exit delay), for example if a peg breaks. It deposits again by itself once the price is back between them.</p>`],
    "lab-exitp": ["Exit prices", `<p>The lower and upper prices that trigger the exit. Drag the red lines on the chart.</p>`],
    "lab-exitdelay": ["Exit delay", `<p>How long the price must stay beyond an exit price before the withdrawal, so a brief wick does not close the position.</p>`],
    "lab-auto": ["Auto-optimize", `<p>Finds which settings matter and whether better ones exist, without you picking ranges, and is built not to fool you with settings that only fit the past. It works on the selected blocks, or on every block when none are selected. Two ways to check:</p>
      <p><b>Walk-forward</b> (the default). The period is cut into equal periods, a month each by default (24 over two years). At every period, the settings that matter are tuned on the periods before it only (the last few, each its own fold; 0: all before), then traded on it. Joined up, those periods are what re-tuning every month would have earned, with nothing seen in advance. Which settings to tune is screened once, on the first periods, before any tested one. The table shows, period by period, re-tuning, the current settings, the strategy without its exits and holding; <b>How the tuned settings moved</b> shows whether tuning found something steady or chased noise (a spread over 0.3 of the range). New settings, tuned on the most recent periods, are suggested only if re-tuning beats the bar over most periods <b>and</b> its edge per period (re-tuning less the bar, period by period) is reliably above zero: its average less 1.64 × its spread ÷ √periods. Months swing by more than most edges, so a higher average alone can be luck.</p>
      <p><b>The current settings were picked on this history</b> (on by default; the built-ins were tuned on Jan 2024 – Oct 2026): then every period is in-sample for them and their numbers flatter them, so the bar is the references nothing was tuned on, and the current settings are shown marked in-sample. Untick it only for settings chosen before the period began.</p>
      <p><b>Rotating windows</b>, the other way:</p>
      <p><b>The windows.</b> The last eighth of the period is the <b>exam</b>: nothing runs on it until a candidate has passed everything else. The rest is cut into five windows. Each attempt is judged on a window of its own (the most recent first, a different one every attempt) and searches on the other four, its walk-forward folds. So no window is used to judge twice, and the exam is looked at by two candidates at most; the second must win each half of it.</p>
      <p><b>Attempt 1, the settings that matter.</b> Each setting is moved on its own (half to twice its value, and 5 min to 4 hour candles) while the others stay put. <b>Moves the score</b> is how far that moved the reliable return. Those moving it by at least a point, and by a quarter of the most any did (at most six), are searched together. Settings that only matter together can be missed.</p>
      <p><b>When it does not hold up, other ways:</b> 2, <b>wider ranges</b> (a quarter to four times the value) for what mattered, dropping settings the search showed change nothing (rows marked ×N gave exactly the same result) and adding any whose best sat at an end of its range; 3, <b>small steps</b>, ±30% on the three that matter most, which fits noise less; 4, <b>changes to the blocks</b>, each tried on its own: without an exit or hold block, an optional input left to its default, SMA, EMA or median for one another, Bollinger or Keltner bands, and or or; 5, <b>the best change, then tuned</b>.</p>
      <p><b>The verdict.</b> An attempt's row 1 passes its window when it beats the current settings there and over all its periods, and most of its top five do too; then it takes the exam. Only a pass on both is suggested, with <b>Apply the suggestion</b> (a change to the blocks is applied too). Otherwise: keep the current settings, with what each attempt found. The table shows the attempt that came closest.</p>`],
    "lab-opt": ["Dynamic optimization", `<p>Select blocks (Ctrl or Shift + click their titles), press <b>⚡ Optimize</b>, tick the settings to tune (or <b>Select all</b>, for one block or all of them) and the range for each. Many backtests run at once; the search tries settings across the ranges, then narrows around the best.</p>
      <p><b>Walk-forward.</b> The period the search sees is cut into folds (consecutive slices, 4 by default), and every setting runs on each on its own. It is scored by its <b>reliable return</b>: the average yearly return over the folds less 1.64 × their spread ÷ √folds, a lower bound on what it earns. A setting that made its money in one stretch and lost in the others ranks below one that earned steadily. <b>Folds +</b> counts the folds it was positive in, <b>worst fold</b> is its worst.</p>
      <p><b>Held out.</b> With the last quarter kept back, the search never sees it. The best twelve are then run on it, and ranked by their reliable return over all periods, the held-out quarter included; the verdict compares the best with the current settings there. A best value on the edge of its range is flagged: widen the range and run again.</p>`],
    "lab-tf": ["Candles (timeframe)", "<p>The candle length this block works on. On <b>5 min</b> it sees the replay's own bars. On longer candles (15 and 30 minutes, 1 and 4 hours, a day) its inputs are taken at each candle's close, highs and lows over the whole candle, built in memory from the stored history.</p><p><b>1 min</b> candles are built from the swaps themselves (the last swap of each minute, its high, low and fees). Decisions still come every 5 minutes and read the minute that ends with the bar, so a 1-minute block mainly gives finer indicators: an RSI over 14 one-minute candles, a 30-minute average of 30 closes.</p><p>Its periods still count in hours (a 24-hour average on 4-hour candles averages six closes), and its value holds from one close to the next. A candle counts only once it has closed, so a decision never sees a candle still forming. Mix timeframes freely: a daily trend filter on a 5-minute range, say.</p>"],
    "lab-token": ["Token to watch", "<p>The token whose dollar price this block gives. The list shows every token in the pool's chain's Solidly pools: pickable ones with the liquidity behind their price (the thinner hop of the route), greyed ones without a pool of at least $500 active liquidity.</p><p>History for the token's pools is downloaded and kept like the pool's own. Each cycle the route is worked out again, so new pools join and pools that thin out drop out.</p>"],
    "lab-park": ["Park unused tokens", `<p>With this on (and <b>Park waiting tokens</b> on under Settings), whatever this pool holds and does not use earns in its pegged pool from <b>Settings › Parking whitelist</b> (USDC in USDC/USDT, WETH in cbETH/WETH, …) until a move needs it: the token it waits in after an exit (both when it keeps them as they are), what its ranges left over, and the part of its budget it has not drawn from the funding tokens yet. It comes back before the strategy re-enters, moves or switches sides. Switch this pool off or set it to 0%, and what it parked is handed over to the pool it sits in, when that pool runs a strategy of its own: it counts as deployed there and that strategy manages it.</p>
      <p>In a backtest a parked token earns what its pegged pool paid each day (its fees and emissions over its active liquidity, ours included), and nothing before that pool's history begins: it could not have parked there. It parks the day the pool appears, as the live app would.</p>
      <p>When the parking pool runs a strategy of its own here (the USDC/USDT ladder, say), the tokens are handed to that strategy as a claim: it places them with its own, and an unpark pays the claim back. Otherwise they are tracked as lots of this pool in a range of their own, apart from anything the wallet already holds in the parking pool: only the lot's own liquidity is withdrawn. Parking also needs <b>Park waiting tokens</b> on under Settings, and skips amounts under the minimum there.</p>
      <p>In backtests the waiting token earns the parking pool's average fee APR over the period, and each move in or out costs two transactions and a swap of half its value.</p>`],
    "lab-r-park": ["Parked yield", `<p>What the waiting token earned in its pegged pool while the strategy was out (backtest: that pool's average fee APR over the period, before our own size dilutes it).</p>`],
    "set-parking": ["Parking whitelist", `<p>For each token, the pegged pool it may wait in. Pick from the pegged pools the market list knows; one pool per token and chain.</p>
      <p>A strategy that exits (for example <b>Wait out the move</b>) and has parking on deposits the token it waits in here (both tokens, when it keeps them as they are), in a range ± the parking range around the price, and takes it back out before it re-enters its own pool.</p>`],
    "set-parking-how": ["How parked tokens are held", `<p>For a pegged pool with no ALM of its own here, each whitelisted token picks how its parked lots sit in it.</p>
      <p><b>Fixed range</b> mints one range ± the width around the price at the time of the park and leaves it there until the tokens are needed back. <b>Re-centre when out of range</b> moves a lot back around the price once the price has stayed outside it for the minutes set (a withdrawal, a swap back and a new park: about four transactions).</p>
      <p>The width is half the range, in percent of the price; empty uses the Parking range below. A pool that runs its own ALM keeps parked lots apart from its positions on a fixed range: that ALM never moves them, and an unpark withdraws only the lot's own liquidity.</p>`],
    "set-parking.enabled": ["Park waiting tokens", `<p>The master switch for parking (off by default). Each strategy also opts in on its own, in the Strategy lab.</p>`],
    "set-parking.min_usd": ["Min value to park", `<p>A park is two transactions in and two out. Below this value the tokens just wait in the wallet.</p>`],
    "set-experimental.enabled": ["Experimental features", `<p>Off by default. Shows the <b>Experimental</b> tab: the groundwork of the AI agent (docs/AGENT_PLAN.md), starting with the Liquidity Lens data it will read and the local cache's request counts.</p>
      <p>The Liquidity Lens figures are <b>placeholders</b> (generated) until each feature is integrated for real. Nothing that trades reads anything here, so turning this on changes no behaviour.</p>`],
    experimental: ["Experimental", `<p>Work in progress behind <i>Experimental features</i> (Settings). Every Liquidity Lens figure here is generated placeholder data in the shape the real service will answer, so the agent and this page can be built before the features arrive.</p>`],
    "exp-tokens": ["Tokens · price, depth, cost", `<p>Per token of your pools: the reference price (a time-weighted median), how many dollars can be bought or sold within 1% of it, the cost of selling $1k, $10k and $100k, and the gap between the spot and the executable price for $10k now and over the last 24 hours. A gap three times its 24-hour average is flagged as widening. Placeholder data.</p>`],
    "exp-pools": ["Pools · traffic, competition, ranges", `<p>Per pool: last week's volume split into aggregator, bot and organic traffic; the other pools of the same pair on other DEXes with their TVL and liquidity within ±0.5% of the price; the best-ranked range width with its expected fee APR; and what another $100k of capital would earn a year. Placeholder data.</p>`],
    // ---- Experimental tab: buttons, fields, status rows and columns ----
    "xb-review": ["Run a review now", `<p>Asks for a review within a minute, at medium effort, outside the schedule. It builds a fresh snapshot, sends it to Claude with the tools, and ends with hold, escalate or a plan. It costs API credit (see API budget) and is refused while the agent is off or another review runs.</p>`],
    "xb-sync": ["Sync history", `<p>Tops up the local event store now: 61 days of swaps for every managed pool and the candidate pools (the pegged pools with the best 24 h APR on your chains). It normally runs on its own: managed pools every 6 hours, candidates daily. Downloads come from the Solidly data processor, only the days missing.</p>`],
    "xb-verify": ["Check the journal", `<p>Re-reads the whole decision journal and checks its hash chain: every record carries the hash of the one before it, so an edited or deleted record breaks the chain at that point. Shows "intact" with the record count, or the first broken record.</p>`],
    "xb-agrefresh": ["Refresh", `<p>Reloads the agent's status, plans, reviews, alerts and journal now. The tab also refreshes itself every 30 seconds while it is open.</p>`],
    "xb-cog": ["Agent settings", `<p>Opens every setting of the agent, the risk controller, the engine caps, the limits and the execution bounds in one window. Saving applies them at once; changing the mode, the limits or the execution bounds voids the plans waiting for approval.</p>`],
    "xb-expref": ["Refresh", `<p>Reloads the Liquidity Lens figures and the cache statistics. The data is placeholder data, generated from the app's own prices, until the real features arrive.</p>`],
    "xf-apikey": ["Anthropic API key", `<p>The key the agent uses to call Claude (from console.anthropic.com → API keys, starting sk-ant-). Each review is billed to that account. Paste it and press Save: it is checked with a free model listing first, then stored encrypted for your Windows account. It is never shown again and is sent only to api.anthropic.com.</p>`],
    "xb-keysave": ["Save the API key", `<p>Checks the pasted key against api.anthropic.com (a free model listing) and stores it if it works. A key that fails the check is not stored.</p>`],
    "xb-keytest": ["Test the API key", `<p>Checks the stored key again (free): useful after a billing or permission change on the Anthropic account.</p>`],
    "xb-keyremove": ["Remove the API key", `<p>Deletes the stored key. Reviews stop until a new one is saved; the risk controller and the engine caps keep working without it.</p>`],
    "xf-bot": ["Telegram bot token", `<p>The token of a bot you create with @BotFather in Telegram (digits, a colon, letters). The app uses it to send alerts and plans for approval, and to read your replies. It is checked with Telegram first, then stored encrypted.</p>`],
    "xb-botsave": ["Save the bot token", `<p>Checks the token with Telegram (getMe) and stores it. An empty field removes the token and unlinks the chat.</p>`],
    "xb-link": ["Link a chat", `<p>Shows a 6-digit code valid for 10 minutes. Send it to your bot in a private chat: that chat and that Telegram user become the only ones the bot listens to. Afterwards the bot sends alerts and plans with Approve / Reject buttons and answers /status, /plans, /pause and /resume.</p>`],
    "xb-unlink": ["Unlink", `<p>Forgets the linked chat. Plans waiting for approval are cancelled, since an approval could have come from that chat.</p>`],
    "xb-approve": ["Approve", `<p>Queues the plan. Approval is bound to the plan's hash: a plan that changed since it was shown is not approved. A queued plan is checked once more right before it starts (its age, prices since the snapshot, locks) and then runs step by step within the execution cost limit.</p>`],
    "xb-reject": ["Reject", `<p>Rejects the plan: nothing in it runs. The journal keeps it, and the next review sees it was rejected.</p>`],
    "xb-cancel": ["Cancel", `<p>Cancels a queued plan before it starts. Once a plan is executing it cannot be cancelled here: pause the app (header) to stop transactions.</p>`],
    "xb-lift": ["Lift the lock", `<p>Shown once the lock's condition has cleared. Lifting it lets deposits and plan steps run on that pool or chain again. Without you, it lifts after the lock time (Agent settings) once cleared.</p>`],
    "xb-freeze": ["Start the forward test", `<p>Freezes each enabled pool's strategy and capital now (with a hash in the journal) as the baseline the agent is measured against. Pressing it again restarts the test and resets the report.</p>`],
    "xb-report": ["Rebuild the report", `<p>Rebuilds the scored report now instead of waiting for the daily rebuild: two backtests per pool (the frozen strategy and the ladder over the test period), at most two at a time.</p>`],
    "xs-agent": ["Agent", `<p>The agent's mode: off (no reviews), advise (reviews run, every plan waits for your approval) or auto (plans that pass the policy gate run on their own). "reviewing…" shows while a review is in progress.</p>`],
    "xs-risk": ["Risk controller", `<p>Its mode (off, monitor, act) and how long ago it last ran. It runs every minute while on; if it has not run within the heartbeat time, the write gate refuses every transaction.</p>`],
    "xs-caps": ["Engine caps", `<p>Whether the caps on rebalances, turnover and execution cost apply to every strategy (Agent settings).</p>`],
    "xs-budget": ["API budget", `<p>What is left of this month's API allowance (the monthly budget plus what carried over from last month), and the calls made this month. Reviews stop when it runs out.</p>`],
    "xs-last": ["Last review", `<p>When the last review ran, what started it (daily, routine, trigger, manual) and how it ended.</p>`],
    "xs-next": ["Next reviews", `<p>The schedule: the daily review after its UTC hour, a routine review every few hours, and the triggers (risk actions, ended plans, invalidations) waiting for the next review.</p>`],
    "xs-open": ["Open plans", `<p>Plans not finished yet: proposed, validated, waiting for approval, queued or executing.</p>`],
    "xs-keys": ["API key and Telegram", `<p>Whether an Anthropic API key is stored, and whether a Telegram chat is linked.</p>`],
    "xh-plan": ["Plan", `<p>The plan's id and when it was submitted.</p>`],
    "xh-state": ["State", `<p>Where the plan is: proposed, validated, awaiting approval, queued, executing, then confirmed, or partially completed, failed, superseded (an emergency took over), expired, cancelled or rejected. While it runs: steps done of all.</p>`],
    "xh-summary": ["Summary", `<p>The agent's one-line summary of the plan.</p>`],
    "xh-changes": ["Changes", `<p>What the plan does to each pool: a strategy change, a new capital target, a new pool with its start capital, or an exit.</p>`],
    "xh-why": ["Why / checks", `<p>For a rejected plan, the checks it failed. For one waiting for approval, why it needs you (advise mode, a token not approved yet, an exit cost that could not be checked, an overfit warning).</p>`],
    "xh-pool": ["Pool", `<p>The pool, by its name in the Pools tab.</p>`],
    "xh-capital": ["Capital", `<p>The pool's capital now, in USD: its positions and its own idle tokens.</p>`],
    "xh-ret": ["Return", `<p>The pool's return over the window, net of deposits and withdrawals (modified Dietz). In brackets: against holding the tokens it had at the start of the window, so positive means the strategy beat holding.</p>`],
    "xh-when": ["When", `<p>When it happened (your local time).</p>`],
    "xh-kind": ["Kind", `<p>What started the review and its effort: daily (high), routine (low; may only hold or escalate), trigger or severe trigger, manual.</p>`],
    "xh-outcome": ["Outcome", `<p>How the review ended: hold decision, escalated (an alert to you), plan submitted or rejected, incomplete (no decision in time), refused, API error, budget skipped or skipped.</p>`],
    "xh-detail": ["Detail", `<p>The reason the agent gave, or what went wrong.</p>`],
    "xh-cost": ["Cost", `<p>What the review cost in API usage (input, output, cached tokens and web searches), in USD.</p>`],
    "xh-tools": ["Tools", `<p>How many tools the agent called in the review (pool data, quotes, backtests, notes). Capped by Agent settings.</p>`],
    "xh-cond": ["Condition", `<p>What the risk controller found: stale data, sources that disagree, a depeg, a loss against holding, low gas, a stuck transaction. Severe ones lock the pool (and in act mode may exit it).</p>`],
    "xh-where": ["Where", `<p>The pool the lock covers, or the whole chain.</p>`],
    "xh-reason": ["Reason", `<p>Why the lock was set.</p>`],
    "xh-cleared": ["Cleared", `<p>When the lock's condition went away. Until then it cannot be lifted.</p>`],
    "xh-actual": ["Actual", `<p>What the pool really returned since the forward test started, net of deposits and withdrawals.</p>`],
    "xh-frozen": ["Frozen", `<p>What the strategy frozen at the start would have returned over the same period (backtest): the result without the agent.</p>`],
    "xh-ladder": ["Ladder", `<p>What the app's ladder strategy would have returned over the same period (backtest).</p>`],
    "xh-hold": ["Hold", `<p>What holding the pool's tokens would have returned over the same period.</p>`],
    "xh-edge": ["Edge", `<p>Actual minus frozen: what the agent added or cost on this pool, before the API spend.</p>`],
    "xh-rules": ["Rules manager", `<p>The deterministic research manager: the frozen strategy re-tuned every week by the auto optimizer on the four weeks before, then traded for the week (walk-forward backtest over the forward test). It is what the app's own optimizer would have done without the agent.</p><p>Shown once the forward test has run a full week.</p>`],
    "xh-edge-rules": ["Edge vs rules", `<p>Actual minus the rules manager: what the agent's research added over the optimizer's own, before the API spend. This is the comparison that says whether the agent is worth running.</p>`],
    "xh-alert": ["Alert or journal record", `<p>An alert (severe, normal or info) or a record of the decision journal, newest first. The journal holds every review, plan state, risk action, approval and settings change.</p>`],
    "xh-token": ["Token", `<p>A token of your configured pools, on its chain.</p>`],
    "xh-ref": ["Reference", `<p>Liquidity Lens's on-chain reference price for the token (placeholder). For the agent it never outvotes the exchange prices.</p>`],
    "xh-depth": ["Depth ±1%", `<p>How many dollars could be bought or sold before the price moves 1%, across the chain's pools (placeholder).</p>`],
    "xh-sell": ["Sell cost", `<p>What selling $1k, $10k and $100k of the token would cost in basis points, by executable quotes (placeholder).</p>`],
    "xh-gap": ["Spot vs exec gap", `<p>The gap between the spot price and what a $10k trade really gets, against its 24-hour baseline. A widening gap is an early sign of thinning liquidity (placeholder).</p>`],
    "xh-vol7": ["7d volume", `<p>The pool's swap volume over 7 days (placeholder).</p>`],
    "xh-mix": ["Traffic mix", `<p>Who trades in the pool: aggregators, bots and organic users, as shares of volume. Organic flow pays fees without arbitrage loss (placeholder; needs Liquidity Lens request R5).</p>`],
    "xh-comp": ["Competing pools", `<p>Pools on other DEXes with the same pair: their fee, TVL and liquidity within ±0.5% of the price, which competes for the same flow (placeholder).</p>`],
    "xh-best": ["Best range", `<p>The range width with the best expected fee APR at today's flow (placeholder).</p>`],
    "xh-earn": ["+$100k capital earns", `<p>What another $100k in the pool would earn a year at today's flow, after diluting the fee share (placeholder).</p>`],
    "xh-src": ["Source", `<p>An external data source read through the local cache.</p>`],
    "xh-today": ["Today", `<p>Requests sent to the source today, answers served from the cache, and failures.</p>`],
    "xh-hit": ["Hit rate", `<p>The share of today's reads served from the local cache instead of the network.</p>`],
    "xh-week": ["7 days", `<p>Requests and cache hits over the last 7 days.</p>`],
    "xh-disk": ["On disk", `<p>The size of the source's cache files.</p>`],
    "set-experimental.agent.model": ["Model", `<p>The Claude model id used for reviews (default claude-opus-5-5). Budget figures assume its prices.</p>`],
    "set-experimental.agent.web_search": ["Web search", `<p>Lets the agent search the web during a review (news, depegs, exploits), billed per search.</p>`],
    "set-experimental.agent.searches_per_review": ["Searches per review", `<p>The most web searches one review may make.</p>`],
    "set-experimental.agent.max_tool_calls": ["Tool calls per review", `<p>The most tools one review may call; past it the review ends as incomplete.</p>`],
    "set-experimental.agent.review_minutes": ["Minutes per review", `<p>The longest a review may run; past it the review ends as incomplete.</p>`],
    "set-experimental.risk.stale_min": ["Stale data", `<p>Minutes without fresh pool data before the risk controller treats it as stale and locks the pool.</p>`],
    "set-experimental.risk.disagree_bp_stable": ["Price disagreement, stables", `<p>How far (bp) a stablecoin pair's pool price may sit from the exchanges' price before it counts as disagreement.</p>`],
    "set-experimental.risk.disagree_bp_eth": ["Price disagreement, ETH pairs", `<p>The same for pairs of ETH and its staked forms (cbETH, wstETH…).</p>`],
    "set-experimental.risk.disagree_bp_other": ["Price disagreement, others", `<p>The same for every other pair.</p>`],
    "set-experimental.risk.peg_pct": ["Depeg", `<p>How far (%) a stablecoin may trade from $1 on the exchanges before it counts as depegged.</p>`],
    "set-experimental.risk.loss_24h_pct": ["Loss vs hold, 24 h", `<p>How much (% of capital) a pool may trail holding over 24 hours before the risk controller acts.</p>`],
    "set-experimental.risk.loss_7d_pct": ["Loss vs hold, 7 d", `<p>The same over 7 days.</p>`],
    "set-experimental.risk.heartbeat_s": ["Write gate heartbeat", `<p>Seconds the risk controller may go without running before the write gate refuses every transaction (while it is on).</p>`],
    "set-experimental.risk.lock_hours": ["Lock lifts after", `<p>Hours after a lock's condition clears before it lifts on its own. 0: only you lift it (Lift, under Safety).</p>`],
    "set-experimental.risk.stuck_tx_s": ["Stuck transaction", `<p>Seconds a sent transaction may stay pending before it counts as stuck and is replaced.</p>`],
    "set-experimental.risk.emergency_gwei": ["Max fee replacing a stuck tx", `<p>The highest fee (gwei) used to replace a stuck transaction.</p>`],
    "set-experimental.caps.turnover_pct_day": ["Cap: turnover a day", `<p>The most a pool may add to positions in a day, as % of its capital. A move past it waits.</p>`],
    "set-experimental.caps.cost_pct_7d": ["Cap: execution cost, 7 d", `<p>The most a pool may spend on gas and swaps over 7 days, as % of its capital.</p>`],
    "set-experimental.limits.eth_pct": ["Limit: ETH", `<p>The most of the account a plan may leave in ETH and its staked forms.</p>`],
    "set-experimental.limits.btc_pct": ["Limit: BTC", `<p>The most in BTC tokens.</p>`],
    "set-experimental.limits.other_pct": ["Limit: other tokens", `<p>The most in tokens that are neither dollars, ETH nor BTC.</p>`],
    "set-experimental.limits.issuer_pct": ["Limit: one issuer", `<p>The most behind one issuer (Circle, Tether, Coinbase, Lido…). The first safe asset (USDC) is not counted.</p>`],
    "set-experimental.limits.lst_pct": ["Limit: LSTs", `<p>The most in liquid staking tokens (cbETH, wstETH, rETH…).</p>`],
    "set-experimental.limits.pool_pct": ["Limit: one pool", `<p>The most of the account in a single pool.</p>`],
    "set-experimental.limits.chain_pct": ["Limit: one chain", `<p>The most of the account on a single chain.</p>`],
    "set-experimental.limits.new_pool_pct": ["New pool start", `<p>The most a new pool may start with, as % of the account.</p>`],
    "set-experimental.limits.new_pool_days": ["New pool: history needed", `<p>Days of stored history a pool needs before a plan may add it.</p>`],
    "set-experimental.limits.actions_per_pool_day": ["Plans per pool a day", `<p>The most plans that may touch one pool in 24 hours.</p>`],
    "set-experimental.limits.no_reversal_h": ["No reversal within", `<p>Hours during which a plan may not put back a strategy it just replaced.</p>`],
    "set-experimental.execution.max_emergency_bp": ["Emergency exit cost", `<p>The most an emergency exit's swaps may cost (bp). Also the cost used for the exit capacity check.</p>`],
    "set-experimental.execution.quote_age_s": ["Quote age", `<p>The oldest a swap quote may be when a plan step uses it.</p>`],
    "set-experimental.execution.plan_age_h": ["Plan expires after", `<p>Hours after its snapshot before a plan that has not started expires.</p>`],
    "set-experimental.execution.price_move_pct": ["Price move voids a plan", `<p>A queued plan does not start if a token's price moved more than this (%) since its snapshot.</p>`],
    "ag-status": ["Agent status", `<p>The agent's parts and whether each is on. Each has its own switch under Agent settings, on top of Experimental features:</p><ul><li><b>Risk controller</b> (off / monitor / act): checks every minute for stale data, price sources that disagree, depegs, losses against holding, low gas and stuck transactions. Monitor alerts and locks pools; act also exits a pool to USDC in an emergency. While it is on, nothing is signed if it has not run for the heartbeat time.</li><li><b>Engine caps</b>: limits on rebalances, turnover and execution cost for every strategy.</li><li><b>Agent</b> (off / advise / auto): reviews by Claude. Advise: every plan waits for your approval. Auto: plans that pass the policy gate run on their own.</li></ul><p><b>Run a review now</b> starts one within a minute. <b>Check the journal</b> verifies the hash chain of the decision record.</p>`],
    "ag-keys": ["API key and Telegram", `<p>The Anthropic API key is checked (a free model listing) and stored encrypted for your Windows account; it is sent only to api.anthropic.com. The Telegram bot token is stored the same way. <b>Link a chat</b> gives a 6-digit code to send your bot in a private chat within 10 minutes; afterwards the bot listens to that chat and user only, sends alerts and plans with Approve / Reject buttons, and accepts /status, /plans, /pause and /resume. Unlinking voids every pending approval.</p>`],
    "ag-plans": ["Plans", `<p>Every plan the agent submitted, newest first. A plan is checked by code (the policy gate: limits, exposure after the plan, exit capacity, backtests of every strategy change) and then rejected, queued, or waits for your approval. Approval is bound to the plan's hash: a plan that changed is not approved. A queued plan is re-checked right before it starts (age, prices since the snapshot, locks) and runs step by step: exits and decreases first, then strategy changes, then deposits, each within the execution cost limit.</p>`],
    "ag-safety": ["Safety", `<p>The risk controller's last check and what it found, and the safety locks. A lock stops deposits and plan steps on a pool or chain (withdrawals and exits still run). It lifts when its condition has cleared and you press <b>Lift</b>, or after the lock time once cleared.</p>`],
    "ag-perf": ["Performance", `<p>Each pool's return over 24 hours, 7 and 30 days, net of deposits and withdrawals (modified Dietz), from the worker's snapshots; in brackets, against holding the tokens it started the period with.</p>`],
    "ag-reviews": ["Reviews", `<p>Each review: when, why (daily, routine, a trigger, manual) and at what effort, its outcome, what it cost in API usage and how many tools it called. A routine review may only hold or escalate. The whole exchange of each review is kept in data/agent/reviews.</p>`],
    "ag-forward": ["Forward test", `<p>Starting the forward test freezes each enabled pool's strategy and capital (with a hash in the journal). Once a day the report backtests the frozen strategy and the ladder over the same period and compares them, and holding, with what really happened. The edge is actual minus frozen, less the API spend. Verdict after 30 days: keep (positive on two thirds of the pools and in total), extend (positive in total only) or stop.</p>`],
    "ag-alerts": ["Alerts and journal", `<p>Alerts the agent raised and the latest records of the decision journal (reviews, plan states, risk actions, approvals, settings changes), newest first.</p>`],
    "ag-settings": ["Agent settings", `<p>Every number the risk controller, the policy gate and the scheduler use. Changing the mode, the limits or the execution bounds voids the plans waiting for approval, as they were checked against the old values.</p>`],
    "set-experimental.agent.mode": ["Agent mode", `<p>Off: no reviews. Advise: reviews run and every plan waits for your approval. Auto: a plan that passes the policy gate and needs no approval for another reason (a new token, an unverified exit) runs without you.</p>`],
    "set-experimental.risk.mode": ["Risk controller", `<p>Off, monitor (alerts and safety locks) or act (also emergency exits to USDC, capped at the emergency cost). While on, the write gate refuses to sign anything if the controller has not run within the heartbeat time.</p>`],
    "set-experimental.caps.enabled": ["Engine caps", `<p>Limits for every strategy: the pool's own rebalances a day, turnover a day and execution cost over 7 days as a share of capital. A move that would break one waits.</p>`],
    "set-experimental.llens": ["Liquidity Lens", `<p>Placeholder: generated data with the shape of what Liquidity Lens will serve, labelled as such and never decision grade. Off: not read at all.</p>`],
    "set-experimental.agent.budget_usd_month": ["API budget", `<p>Dollars of Claude API usage a month. What is left carries into the next month (one month's worth at most). Each call reserves its worst case first and is not made when it does not fit. At zero the agent stops until next month.</p>`],
    "set-experimental.agent.reserve_pct": ["Budget reserve", `<p>The share of the month's budget kept for the daily review and severe triggers: routine and normal reviews stop before they touch it.</p>`],
    "set-experimental.agent.daily_utc_hour": ["Daily review", `<p>The hour (UTC) after which the day's main review runs, at high effort.</p>`],
    "set-experimental.agent.routine_h": ["Routine reviews", `<p>Hours between low-effort routine reviews, which may only hold or escalate.</p>`],
    "set-experimental.limits.move_pct_day": ["Capital moved a day", `<p>The most capital plans may move in 24 hours, as a share of the account.</p>`],
    "set-experimental.limits.exit_capacity_pct": ["Exit capacity", `<p>The share of the non-USD holdings after a plan that must be sellable to USDC within the emergency cost, by live quotes.</p>`],
    "set-experimental.execution.max_cost_bp": ["Plan step cost", `<p>The most a plan step's swaps may cost, in basis points; a plan may ask for less, never more. While a step runs, the pool trades within it.</p>`],
    "exp-cache": ["Local cache", `<p>Every external source is read through a local cache: downloaded on first use, then only topped up. Per source: requests sent and answers served from the cache today, the hit rate, the last 7 days and the size on disk. After two failures in a row a source is left alone for 5, then 15, then 60 minutes.</p>`],
    "set-parking.range_pct": ["Parking range", `<p>How wide the parking position is, as ± percent of the pegged pool's price (0.5 = ±0.5%). Narrow earns more of the fees; wide stays in range through a small depeg.</p>`],
    "lab-exitto": ["Withdraw into", `<p>What the exit leaves in the wallet: both tokens as withdrawn, everything swapped into one of them (costs a swap), or <b>all in USDC</b>: both tokens sold for the chain's dollar stable, which waits parked (when USDC is on the parking whitelist) and buys the pair back on re-entry through the funding step. USDC must be a funding token on the chain.</p>`],
    "lab-guard": ["Depeg guard", `<p>Freezes all action while the price is more than this many ticks from its 7-day median. The position is kept as it is; nothing is moved or withdrawn until the price returns.</p>`],
    "lab-costs": ["Costs and limits", `<p>What the backtest charges and credits:</p><ul><li><b>Swap cost</b>: basis points paid on the value swapped at each move.</li><li><b>Swap to/from USDC</b>: the same for an exit into USDC and its buy-back, which go through the aggregator to another pair and can cost more in a fast market (quotes on Base for $1k to $8k round-tripped at about 0 bp in October 2026; 1 bp by default).</li><li><b>Gas</b>: dollars per transaction (withdraw, swap, deposit).</li><li><b>Spot check</b>: the worker's own safety check, modelled. A move or deposit waits while the price is more than this many ticks from the median of the last hour's swaps (a possible manipulation or a fast move); exits never wait. It starts at the pool's own setting, so the backtest blocks where the worker would. 0 turns it off.</li><li><b>Rebalances per UTC day</b>: the worker's daily cap, modelled. A move past it waits for the next UTC day (exits never do); the results show how many moves it blocked ("capped"). Empty runs without a cap, to see what the cap costs.</li><li><b>Max swap cost</b>: the dearest swap the worker makes. The backtest still makes dearer ones and counts them, as a note under the results.</li><li>Both start at the pool's own limits (Pools › ⚙). <b>Save limits to</b> the pool sets its live limits to the ones here (live always has a daily cap).</li>
      <li><b>Fees received</b>: the share of the processor's fee attribution that is really paid out (measured at 82% on this wallet).</li><li><b>Emissions value</b>: what SOLID emissions are worth when sold (it has little liquidity).</li></ul>`],
    "lab-chart": ["Price and strategy", `<p>The pool's price over the period (grey), the ranges the backtest held (green blocks) and its moves (ticks at the bottom).</p>
      <p><b>Node strategies</b>: the dashed green lines are the range the strategy wanted at each hour; the coloured lines are its indicators (toggle them with the chips under the chart).</p>
      <p><b>Simple rules</b>: the strip on the right shows the rules at today's price: the range (green), the trigger band (dashed blue), and the cutoffs (amber) and exits (red) across the chart. Drag any handle; the backtest re-runs when you let go.</p>`],
    "lab-results": ["Backtest results", `<p>Always on the right while you edit. Each card replays the same swaps: the strategy you are editing (outlined), the ones you added with <b>＋ Add a strategy</b>, and holding half and half. The big number is the profit, then its yearly rate; the small figures split it up. The chart below shows each run's value over time.</p>
      <p><code>value − holding = fees + emissions − swap costs − gas + IL</code></p>
      <p>The best figure across the cards is green. Volume is held fixed, which is conservative; fees and emissions are paid out, not compounded.</p>`],
    "lab-overfit": ["Overfit check", `<p>After each full backtest the lab runs a second step for the strategy you are editing: the same swaps replayed with every setting nudged between half and double (24 neighbours), and the period cut into 8 time slices.</p>
      <p><b>Walk-forward efficiency</b>: the best settings on the slices so far, measured on the next slice, over what they earned in sample (1 = held up). <b>PBO</b>: over the 70 ways to split the slices into halves, how often the best settings of one half rank in the bottom half of the other. <b>Neighbours holding up</b>: share of nudged settings keeping at least half the edge over holding. <b>Slices beating hold</b>: how steady the edge is over time.</p>
      <p>Score = 35% (1 − PBO) + 25% efficiency + 25% stability + 15% consistency. 70+ robust, 45–70 some risk, below 45 likely overfit. A strategy with no settings to vary gets no score. Short periods give noisy slices.</p>`],
    "lab-r-value": ["Value at the end", `<p>Everything the strategy holds at the end (the positions at the final price, idle tokens, and the fees and emissions it was paid), in the numeraire.</p>`],
    "lab-r-pnl": ["Profit", `<p>Value at the end minus the capital, and the same as a yearly rate. It includes the price change of the tokens held.</p>`],
    "lab-r-hodl": ["vs holding", `<p>Value at the end minus what the first deposit's token amounts would be worth if simply held. This is what the strategy added on top of owning the tokens.</p>`],
    "lab-r-fees": ["Fees", `<p>The strategy's share of every swap's fee while its range was active at the swap's price, after the fee haircut.</p>`],
    "lab-r-emis": ["Emissions", `<p>SOLID emissions earned by time in range, in proportion to the liquidity held, valued with the emission haircut.</p>`],
    "lab-r-costs": ["Costs", `<p>Swap costs at each move (on the value swapped) and gas for every transaction.</p>`],
    "lab-r-il": ["IL", `<p>Impermanent loss and rebalancing drift: how much the positions' tokens lost (or gained) against holding the first deposit, as the price moved and the ranges were moved. Swap costs are counted separately.</p>`],
    "lab-r-moves": ["Moves", `<p>How many times the strategy withdrew and re-deposited (or exited).</p>`],
    "lab-r-tir": ["Time in range", `<p>The share of the period the strategy's liquidity was at the pool's price, earning.</p>`],
    "lab-moves": ["Moves and exits", `<p>Every move of your rules in the backtest, with its new range and the reason the rules gave, the same reasons the worker logs live.</p>`],
    "lab-apply": ["Using a strategy", `<p>The lab is for building and testing. To run a strategy on a pool, <b>Save</b> it here, then pick it in the pool's row under <b>Pools</b> (expand the row): the pool gets a copy fitted to its tick spacing, with the strategy's settings right there to adjust, and <b>Save</b> in the bar applies it from the next cycle.</p>`],
    "p-strategy-pick": ["Pool strategy", `<p>Pick the strategy this pool runs: the ladder (the app's default), a built-in simple-rules or indicator strategy, or one you saved in the Strategy lab. The pool gets its own copy, fitted to its tick spacing (simple rules given in spacings become ticks; a ladder gets the defaults for the pair).</p>
      <p>The fields below change with the strategy: the ladder's parameters, the simple rules (width, placement, buffers, timers, cutoffs, exits, depeg guard), or each block's settings of a node strategy. Edits apply to this pool only and are saved with the bar below; <b>Open in the lab</b> backtests the pool's current strategy. Unfinished strategies (saved with an unconnected block) cannot be picked.</p>`],
    "lab-ladder-form": ["Ladder settings", `<p>The ladder's own parameters. Empty fields use the pool's defaults.</p>`],

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
      <p>When a pool's idle tokens exceed <i>Redeploy idle above</i> (Settings, default 2% of its capital), the next cycle tops the position up without withdrawing it. Smaller amounts wait for the next rebalance.</p>`],
    "k-profit": ["Profit", `<p>What the app has made for you across all chains, from its own transactions:</p>
      <p><code>profit = earned − gas − swaps − IL</code></p>
      <ul><li><b>Earned</b>: everything the reward claims paid into the wallet, plus what is claimable now, plus what is <i>pending</i> (earned since the last claim snapshot; it becomes claimable with the next one, about daily). On Solidly V3 all LP income (swap fees, SOLID emissions, bribes) is paid through claims.</li>
      <li><b>Gas paid</b>: every transaction the app sent (claims, swaps, approvals, deposits, withdrawals), including the L1 data fee on OP-stack chains.</li>
      <li><b>Swaps</b>: slippage and price paid on rebalancing swaps and SOLID sales (what went out minus what came in). Shown with a + when the swaps came out ahead, which happens when the aggregator beats the pool price.</li>
      <li><b>IL</b> (impermanent loss): what the positions gave back when withdrawn, or hold now, minus what the tokens put into them would be worth if simply held. Counted for liquidity the app added, at the prices of each withdrawal (today's for open positions). On a pegged pair it is usually small; it grows when the price moves through a range.</li></ul>
      <p>Each transaction is valued at the token prices of its time (today's when no snapshot is near). Later price moves of the tokens you hold are not counted here; they show in the managed value. Transactions you sent yourself are not included.</p>`],
    "k-earned": ["Earned", `<p>Claimed rewards (valued at the prices when they were claimed), plus rewards claimable now, plus pending rewards (earned since the last claim snapshot), summed over all chains. The line below splits it into the three.</p>`],
    chaincard: ["Chain", `<p>Everything the app manages on one chain. The same wallet address is used on every chain.</p><ul>
      <li><b>Managed</b>: positions plus idle managed tokens on this chain. Allocations on this chain are percentages of this number.</li>
      <li><b>Deployed / Idle</b>: in positions vs free in the wallet.</li>
      <li><b>Gas balance</b>: the chain's native token (ETH, or S on Sonic). Red, with a warning, when it is below the minimum: the worker then takes no action on this chain until you top it up.</li>
      <li><b>Earned, gas paid, swap costs, profit</b>: as in the profit figure at the top, for this chain only.</li></ul>`],
    chainfilter: ["Chains", `<p>A filter for the pools table: which chains' pools are listed (with their APR windows and fee / arb loss). Showing a chain loads its pool list; hiding it hides its other pools.</p>
      <p>Nothing else depends on it: your own pools are always listed and managed, and wallet balances and funding tokens cover every chain. Fantom is not offered: Solidly retired it.</p>`],
    chain: ["Chain", `<p>The chain the pool is on. Each chain has its own balances, gas and Unallocated: a pool's capital is on its own chain.</p>`],
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
      <li><b>Capital</b>: the pool's own money (positions, idle tokens, what it parked). It keeps what it earns.</li>
      <li><b>Tick · fee</b>: the pool's current price tick and current dynamic fee (pips, 100 pips = 1 bp).</li>
      <li>The position list shows each range [lower tick, upper tick) with a green dot when the current price is inside it (earning fees) and red when outside.</li>
      <li><b>Last decision</b>: what the strategy decided in the last cycle. Open <i>cycle log</i> for the full step-by-step plan.</li></ul>`],
    position: ["Position", `<p>A liquidity range [lower tick, upper tick). Tick t is the price 1.0001^t (token1 per token0); on a pegged pair 1 tick ≈ 0.01%.</p>
      <p>A position only earns fees while the price is inside its range (green dot). The ladder strategy uses 1-tick-wide positions: very capital-efficient while the peg holds, re-solved every few hours as the price moves.</p>`],
    decision: ["Last decision", `<p>What the pool's strategy decided in the last cycle:</p><ul>
      <li><b>keep</b>: nothing to do.</li>
      <li><b>rebalance</b>: withdraw everything and redeploy into new ranges (the ladder re-solved and more than its turnover threshold moved).</li>
      <li><b>topup</b>: add idle tokens to the current ranges without withdrawing.</li>
      <li><b>exit</b>: the pool was switched off, so all positions are withdrawn.</li>
      <li><b>guard</b>: the depeg guard froze the pool because the price is far from its 7-day median.</li>
      <li><b>error</b>: the pass failed. Details are in the log and Activity, and it is retried next cycle.</li></ul>`],
    wallet: ["Wallet (managed tokens)", `<p>Free balances of the tokens the worker manages: every configured pool's two tokens plus the funding tokens.</p>
      <p>Other tokens in the wallet are ignored and never touched, except the reward tokens listed for selling (SOLID).</p>`],
    rewards: ["Rewards", `<p>Claimable amounts per token from Solidly's rewards API (<code>/api/rewards/&lt;wallet&gt;</code>), valued in USD.</p>
      <p>The claim submits the API's Merkle claim object unchanged to <code>rewarder.claimAll</code> after simulating it.</p>
      <p>Compounding: claimed pool tokens join the next rebalance. SOLID is sold into the pool's numeraire during that rebalance, in the largest chunk that loses at most the configured %. SOLID liquidity is thin, so small amounts are kept until they are worth selling.</p>`],

    // ---- pools ----
    capital: ["Capital per pool", `<p>Every pool runs on its own money, its <b>capital</b>: its positions, the tokens it holds idle, and what it parked in other pools. What a pool earns stays in it, and what it loses is its own loss. Nothing is moved between pools by itself, so the shares grow and shrink with each pool's results.</p>
      <ul><li><b>Unallocated</b>: free tokens in the wallet that no pool holds, such as deposits, withdrawals and anything a switched-off pool gave back. Pools never touch it.</li>
      <li><b>Add from Unallocated</b>: moves dollars into a pool, USDC first, then the other stables, then the pool's own tokens. The pool deploys them on its next run.</li>
      <li><b>Withdraw to USDC</b>: tick one pool, several or All, and enter a dollar amount. It is split equally over them: a pool with less gives all it has, and the others make up the rest. Each pool sells its part to USDC when it next runs, from its idle tokens first and then the same slice of each position. The USDC lands in Unallocated, where you can add it to another pool or send it out of the wallet yourself.</li>
      <li><b>Move between pools</b>: a withdrawal from one pool whose USDC goes straight into another. That pool swaps it into its own tokens when it deploys.</li>
      <li><b>Reset to %</b>: a one-off. Every pool's idle tokens go back to Unallocated, then each pool is given what it needs to reach the % you set of everything on the chain. Positions are not touched.</li></ul>
      <p>Requests wait in the list under the bar until they are done, and can be cancelled there. Rewards claimed go to the pools that earned them (the rewards API reports them per pool). Tokens sent out of the wallet shrink every pool's holding of that token in proportion. A pool switched off withdraws its positions and gives its idle tokens back to Unallocated.</p>
      <p>The first time this version runs, each pool keeps what it holds and is filled up from the free tokens to its allocation %, as before.</p>`],
    allocation: "capital",
    blend: ["Blended APR", `<p><b>Blended APR</b> is what the positions in range right now would earn at each pool's income of the last 24 hours (fees, emissions and bribes), over everything on the chain. Our share of a pool's income is our in-range liquidity over the pool's active liquidity. Idle tokens, Unallocated and positions out of range count as 0%.</p>
      <p><b>Realised APR</b> is what the positions actually earned (claimed + claimable + pending rewards) since the first position on the chain, over the average money in positions in that time (from the cycle snapshots), per year. It shows after a day.</p>
      <p>Shown for the last 1 h, 6 h and 24 h, with the yearly dollars at the 24 h rate. These are raw processor figures: the backtests assume 82% of fees are actually paid and SOLID is worth half, so real results tend to be lower. Short windows are noisy, because volume is lumpy.</p>`],
    "p-color": ["Colour", `<p>The pool's colour in the capital bar.</p>`],
    "p-pool": ["Pool", `<p>The pool's name in this app, its token pair and its contract on the chain's explorer.</p>`],
    "p-enabled": ["Enabled", `<p>A disabled pool gets no new deposits. If it still holds positions, the next cycle withdraws them, and its idle tokens then go back to Unallocated. Use it to exit a pair without deleting its settings.</p>`],
    "p-alloc": ["Capital", `<p>The pool's own money: its positions, idle tokens and what it parked (hover for the parts). Below it: its share of the pools' capital on the chain, and what is in positions. <b>Add</b> moves money in from Unallocated; <b>Withdraw</b> takes it out to Unallocated as USDC. The switch enables or disables the pool; the ⚙ button opens its strategy parameters, Re-solve and Remove.</p>`],
    "p-target": "capital",
    "p-deployed": ["Deployed", `<p>The value of this pool's positions right now.</p>`],
    apr: ["Estimated APR (if deposited)", `<p>What a deposit would have earned over the window, annualised:</p>
      <p><code>(fees + emissions + bribes) × 8760 / hours ÷ (in-range TVL + deposit)</code></p>
      <ul><li><b>Inputs</b>: fees, emissions and bribes are what the whole pool paid its in-range LPs, from the Solidly data processor's hourly history.</li>
      <li><b>Deposit</b>: your deposit joins the in-range liquidity and shares pro rata, like the average in-range LP. In <i>Pools</i> it is the pool's target size, with your own existing liquidity taken out of the TVL so it is not counted twice. A pool at 0% shows a 10% slice. For other pools it is the deposit size you entered.</li>
      <li><b>Windows</b>: 1 h reacts fast but is noisy; 24 h is steadier. Large differences mean volume came in bursts.</li>
      <li><b>Caveats</b>: these are raw figures. The backtests count fees at 82% (what the rewarder actually paid) and SOLID at 50%. The ladder's 1-tick positions usually earn more than the average in-range LP, and wide ranges earn less.</li></ul>
      <p>Hover a value for its breakdown, shown under "This value" below.</p>`],
    "p-strategy": ["Strategy & limits", `<p>The strategy that decides where the liquidity goes, plus this pool's safety limits. Open it with the ⚙ button on the pool's row; changes apply after <b>Save</b>.</p>
      <p>The ladder is a block of a node strategy (open it in the Strategy lab); a ladder from an older version was turned into one with the same settings.</p>
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
    "param-max_spot_dev_ticks": ["max_spot_dev_ticks", `<p>Manipulation and fast-move check: no action while the current tick is further than this from the last hour's median tick. It protects against depositing around a price that was just pushed by a large trade. Exits are never blocked. Backtests model it too (Strategy lab › Costs, the spot check), starting from this value. As a guide: 10 for a stable pair and 50 for an ETH pair like cbETH/WETH each block about 0.01–0.2% of 5-minute checks over the last 90 days.</p>`],
    "param-max_actions_per_day": ["max_actions_per_day", `<p>The most rebalances, top-ups or exits per UTC day for this pool. One rebalance is several transactions. It caps runaway behaviour if something goes wrong.</p>`],
    "param-min_swap_value": ["min_swap_value", `<p>The smallest imbalance (in token0 units) worth a swap. Smaller differences are deposited as they are or left idle. 5 for USD stables, 0.002 for ETH pairs.</p>`],
    "param-max_ranges": ["max_ranges", `<p>The most positions this pool may hold. If the strategy wants more (a volatile pair's ladder can ask for dozens of tick ranges), neighbouring ranges are merged into this many contiguous groups of similar weight.</p>
      <p>Each position costs one deposit and one withdrawal transaction per rebalance, so fewer positions mean cheaper, faster rebalances. Stable pairs normally use 1–4 anyway.</p>`],
    "upd-check": ["Updates", `<p>The app checks GitHub's latest release every 12 hours (Settings › Check for signed updates) and offers it only when the release's checksum file is signed by a key on the signer whitelist built into the app. <b>Update now</b> downloads the installer, checks it against the signed checksum, and installs it; the app stops after its current cycle and starts again.</p><p>A release that is not signed, or signed by another key, is not offered; the reason is shown here.</p>`],
    resolve: ["Re-solve", `<p>Makes this pool's ladder re-solve on the cycle that starts now, instead of waiting for its 6-hour cadence. It only rebalances if the new solution moves more than <i>turnover</i>.</p>`],
    remove: ["Remove", `<p>Deletes the pool from the configuration. Only possible once it holds no positions: switch it off first and let a cycle withdraw it.</p>`],
    savepools: ["Save pools", `<p>Writes enabled flags and strategy settings to <code>config/pools.json</code> and starts a cycle. Money is moved with Add, Withdraw and Move instead.</p>`],
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
    "lab-r-hedge": ["Hedge", `<p>What the strategy's perp short on Hyperliquid added: its price P&L (it gains when the token falls), the funding it earned (shorts are paid while funding is positive) less what it paid, and its trading and transfer fees. The short is the pool's own price, so the perp's basis is not modelled. Its margin comes out of the capital at the start and is counted in the hold baseline.</p><p>The live app does not hedge yet: a strategy with a Hedge block runs in backtests only.</p>`],
    "pos-chart": ["APR per position", `<p>The APR each position earned, measured by the Solidly UI: the fees credited to the position between two samples, over its value, per year. The app samples every 15 minutes, so the solid lines start when this version first ran.</p>
      <p>Before that, the dashed steps come from the Solidly UI's trailing fee sums for each live position (the last hour, 1–2 hours ago, 2–4 hours ago, up to 7–30 days ago), each over today's value. Dotted lines are the SOLID rewards per pool, over the money in the pool.</p>
      <p>Every figure is the LP's share of the fees (80%; the rest is the protocol's). Click a position in the legend or the table to pick it out.</p>`],
    "pos-table": ["Positions", `<p>Each range the wallet holds, and the ranges it closed in this period that earned something (tick for "closed positions"). A range that is withdrawn and minted again counts as a new position.</p>
      <p><b>APR 24 h</b> and <b>7 d</b>: the fees of that window over the position's value now. <b>APR life</b>: the fees over the position's time-weighted capital since it opened, as the Solidly UI shows it.</p>`],
    "refresh-pools": ["Refresh", `<p>Re-reads every chain's pool list and APRs from the Solidly UI now. The list also follows the Solidly UI on its own: the app's copy is at most 5 minutes old, and the table re-reads it every 5 minutes while this tab is open.</p>`],
    refresh: ["Refresh", `<p>Reloads the pool list and the APR windows. Data is cached for a few minutes: pools 5 min, APR windows 10 min.</p>`],

    // ---- settings ----
    "set-interval_s": ["Cycle interval", `<p>Seconds between worker cycles (default 300). Strategies keep their own cadence (the ladder re-solves every 6 h), so a shorter interval mostly means faster reaction to failures, idle tokens and depeg-guard releases. Very short intervals add RPC and aggregator load.</p>`],
    "set-idle_redeploy_pct": ["Redeploy idle above", `<p>When a pool's idle tokens exceed this % of its capital, the next cycle deposits them into the current ranges (a top-up) without waiting for the next rebalance. It recovers failed or partial deposits and money added quickly. Smaller amounts, such as weekly claims, wait for the next rebalance.</p>`],
    "set-resize_tolerance_pct": ["Resize tolerance", `<p>A rebalance changes only what has to change. A position whose range is still in the new target is kept: it is trimmed with a partial withdrawal when it holds too much, or topped up by depositing into the same range when it holds too little. Only ranges that leave the target are withdrawn in full.</p><p>Differences smaller than this % of the pool's capital (and never less than the pool's minimum swap value) are left alone, so small drifts don't cost transactions. Default 1%.</p>`],
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
    "set-ui_port": ["Dashboard port", `<p>The port this dashboard listens on, on 127.0.0.1 only (default 8080). Saving a new one moves the dashboard there at once and this page follows; the old port keeps answering for 20 seconds. A port another program holds is refused and not saved. At startup, if the port is taken, the app uses the next free one (up to 20 higher, else any free port) and the tray's <b>Open dashboard</b> goes there.</p>`],
    funding: ["Funding tokens", `<p>The tokens a pool may swap into its own when it holds them: USDC added to it, say. Example: you add $3,000 of USDC to cbETH/WETH, and it buys cbETH and WETH with it when it deploys.</p>
      <p>Each chain has its own row and list; a pool is only ever funded with tokens on its own chain. Every chain starts with its USDC and USDT. Click a row's tokens to add or remove, then <b>Save</b> that row; <b>Reset</b> puts the chain back to USDC and USDT.</p>
      <p>You can tick several. A shortfall is bought from the selected token with the largest USD balance first; if that runs out, the next one covers the rest. Shortfalls under $10 are ignored.</p>
      <p>The picker shows every token of that chain's Solidly pools with your wallet balance (or paste an address); ticked tokens are listed first, and the closed picker shows them as chips (× removes one). A selected token that is one of the pool's own tokens is not swapped by funding; the normal rebalancing swap handles it. <b>None</b> empties that chain's list and turns cross-pool funding off there: its pools then only use their own tokens.</p>`],
    privkey: ["Wallets", `<p>Every wallet the app runs, each with its own pools, split of funds and statistics. A wallet's address is its key's; a wallet without a key watches the address you give it (<b>Change address</b>) and signs nothing.</p>
      <p><b>Set key</b> stores a private key encrypted for your user account (Windows DPAPI or the macOS Keychain), so the worker resumes after restarts without asking. The API never returns it and logs never contain it. <b>Remove key</b> deletes it; the wallet becomes watched only and live trading turns off.</p>
      <p><b>Go live</b> / <b>Switch to dry run</b> and <b>Pause</b> / <b>Start</b> act on that wallet only.</p>`],
    helpvis: ["Show explanations panel", `<p>Shows or hides this panel. It is on by default. The choice is remembered in this browser only (not a worker setting), and takes effect immediately without saving.</p>
      <p>With the panel hidden, hovering still works as normal tooltips where values have breakdowns.</p>`],
    savesettings: ["Save settings", `<p>Writes the options to <code>config/settings.json</code> (funding tokens and RPCs have their own Save per chain). They take effect on the next cycle.</p>`],

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
    "Fee": "fee", "Pair": "pair", "Chain": "chain", "Pool": "p-pool", "Enabled": "p-enabled", "Allocation": "p-alloc", "Capital": "p-alloc", "Target": "p-target", "Deployed": "p-deployed",
    "Strategy & limits": "p-strategy", "Token": "wallet", "Claimable": "rewards", "Reward": "rewards", "Pending": "rewards", "When": "events", "Event": "events", "Details": "events"};
  const BTN = {"Run now": "run_now", "Claim now": "claim_now", "Pause": "pause", "Resume": "pause", "Go live": "live", "Switch to dry run": "live",
    "Re-solve": "resolve", "Remove": "remove", "Add": "addpair", "Save pools & allocation": "savepools", "Save pools": "savepools", "Discard changes": "discard", "Save settings": "savesettings",
    "Save key": "privkey", "Refresh": "refresh"};

  function tag(root) {
    const set = (el, k) => { if (el && !el.dataset.help) el.dataset.help = k; };
    const XTH = {"Plan": "xh-plan", "State": "xh-state", "Summary": "xh-summary", "Changes": "xh-changes", "Why / checks": "xh-why", "Pool": "xh-pool",
      "Capital": "xh-capital", "24 h": "xh-ret", "7 d": "xh-ret", "30 d": "xh-ret", "When": "xh-when", "Kind": "xh-kind", "Outcome": "xh-outcome",
      "Detail": "xh-detail", "Cost": "xh-cost", "Tools": "xh-tools", "Condition": "xh-cond", "Where": "xh-where", "Reason": "xh-reason", "Cleared": "xh-cleared",
      "Actual": "xh-actual", "Frozen": "xh-frozen", "Ladder": "xh-ladder", "Hold": "xh-hold", "Edge": "xh-edge", "Rules manager": "xh-rules", "Edge vs rules": "xh-edge-rules", "Token": "xh-token", "Reference": "xh-ref",
      "Depth ±1% (buy / sell)": "xh-depth", "Sell $1k / $10k / $100k": "xh-sell", "Spot vs exec gap ($10k)": "xh-gap", "7d volume": "xh-vol7",
      "Aggregator / bot / organic": "xh-mix", "Competing pools (TVL, ±0.5% liquidity)": "xh-comp", "Best range (width → APR)": "xh-best",
      "+$100k capital earns": "xh-earn", "Source": "xh-src", "Today: requests / hits / failures": "xh-today", "Hit rate today": "xh-hit",
      "7 days: requests / hits": "xh-week", "On disk": "xh-disk"};
    // the Experimental tab's tables first: their headers mean something else than the same words elsewhere
    root.querySelectorAll('section[data-tab="experimental"] table').forEach(t => {
      const heads = [...t.querySelectorAll("tr:first-child th")].map(th => XTH[th.textContent.trim()] || null);
      t.querySelectorAll("tr:first-child th").forEach((th, i) => heads[i] && set(th, heads[i]));
      t.querySelectorAll("tr").forEach(tr => [...tr.children].forEach((td, i) => { if (td.tagName === "TD" && heads[i]) set(td, heads[i]); }));
    });
    root.querySelectorAll("#agAlerts td").forEach(td => set(td, "xh-alert"));
    root.querySelectorAll("#tabs button").forEach(b => set(b, "tab-" + b.dataset.t));
    root.querySelectorAll("button").forEach(b => { const k = BTN[b.textContent.trim()]; if (k) set(b, k); });
    root.querySelectorAll("table").forEach(t => {
      const heads = [...t.querySelectorAll("tr:first-child th")].map(th => TH[(th.dataset.label ?? th.textContent).trim()] || null);
      t.querySelectorAll("tr:first-child th").forEach((th, i) => heads[i] && set(th, heads[i]));
      t.querySelectorAll("tr").forEach(tr => [...tr.children].forEach((td, i) => { if (td.tagName === "TD" && heads[i]) set(td, heads[i]); }));
    });
    root.querySelectorAll("#settings [data-k]").forEach(el => set(el.closest("div"), el.dataset.k === "solid_mode" ? "solidmode" : "set-" + el.dataset.k));
    root.querySelectorAll("#pools .fields > div").forEach(d => { const l = d.querySelector("label"); if (l) set(d, "param-" + l.textContent.trim()); });
    root.querySelectorAll(".pdetail .card").forEach(c => set(c, "poolcard"));
    root.querySelectorAll(".pdetail .pos-list > div").forEach(d => set(d, "position"));
    root.querySelectorAll(".pdetail .card > .small").forEach(d => set(d, "decision"));
    root.querySelectorAll("#events tr").forEach(tr => { const c = tr.children[tr.children.length - 2]; if (c && c.tagName === "TD") set(c, "ev-" + c.textContent.replace(/\(dry\)/, "").trim()); });
    root.querySelectorAll(".chip.volatile").forEach(c => set(c, "volatile"));
    root.querySelectorAll("#pools .chip.mine").forEach(c => set(c, "configured"));
    root.querySelectorAll("#allocs .card").forEach(c => set(c, "allocation"));
    root.querySelectorAll("#chainBar .chipbtn").forEach(c => set(c, "chainfilter"));
    const ids = {mode: "mode", cycle: "cycle", chart: "chart", tokens: "wallet", rewards: "rewards", chainBar: "chainfilter", q: "search", depSize: "depsize", minTvl: "mintvl", fundCard: "funding", wList: "privkey", errors: "errors"};
    for (const [id, k] of Object.entries(ids)) set(document.getElementById(id), k);
    const kp = {kTotal: "k-total", kProfit: "k-profit", kEarned: "k-earned", kDeployed: "k-deployed", kIdle: "k-idle"};
    for (const [id, k] of Object.entries(kp)) { const e = document.getElementById(id); if (e) set(e.closest(".card"), k); }
    set(document.getElementById("solidModeBox"), "solidmode");
    set(document.getElementById("solidLpBox"), "solidlppool");
    const hv = document.getElementById("helpVisible"); if (hv) set(hv.closest("label"), "helpvis");
    const os = document.getElementById("onlySupported"); if (os) set(os.closest("label"), "onlysupported");
    const om = document.getElementById("onlyMine"); if (om) set(om.closest("label"), "onlymine");
    set(document.getElementById("allocSum"), "allocation");
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
    let h = H[key] || H._default;
    if (typeof h === "string") h = H[h] || H._default; // an alias
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
