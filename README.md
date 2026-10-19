# Stable ALM

Stable ALM manages concentrated liquidity on **Solidly V3** for you, on every chain Solidly runs on
(Ethereum, Optimism, Base, Arbitrum and Sonic). It runs as a small desktop app
(a tray icon plus a local dashboard in your browser) and keeps your positions where trading
actually happens, so they keep earning swap fees and SOLID emissions with no manual rebalancing.

It is built for **pegged pairs**: two tokens that track the same asset, such as USDC/USDT or
cbETH/WETH. Those are the pairs the strategy was designed and backtested on.

## How it works

Every few minutes the app checks each pool you enabled and decides whether your liquidity should
move.

- **The ladder.** Instead of one wide range, your capital is spread over single ticks around the
  price, weighted toward the ticks where fees were actually paid and where other liquidity is thin.
  The amount per tick comes from a water-filling rule: each tick gets liquidity up to the point
  where one more dollar would earn the same there as anywhere else.
- **It moves rarely.** The ladder is re-solved every 6 hours and only acted on if more than 40% of
  the capital would move. A few legs sitting just outside the price is deliberate: pegged prices
  return, and repositioning on every small move costs more than it earns.
- **Safety first.**
  - The **depeg guard** freezes a pool while its price is more than 15 ticks from its 7-day median.
  - Every transaction is simulated before it is sent.
  - Swaps go through the Solidly aggregator with a cost cap.
  - There is a daily rebalance limit, a gas check and a kill switch.
- **Rewards compound.** Rewards are claimed weekly, and claimed SOLID is sold into the pool (or, as
  an option, put into a SOLID pool).
- **It starts in dry run.** Every cycle plans and simulates against the real chain, but nothing is
  signed until you add a key and press **Go live**.

The dashboard shows your balance, deployed and idle value, gas and **profit** per chain and in total.
Profit is computed from the app's own transactions: what the reward claims paid in (plus what is
claimable now), minus gas and swap costs.

Each pool card on the dashboard also shows **fee vs arbitrage loss (7 days)**: whether liquidity at
the current price earns more in fees than it loses to arbitrage. It is a health signal for the pool;
the help panel explains how to read it.

### Your own strategies: the Strategy lab

Instead of the ladder, a pool can run a strategy you build in the **Strategy lab** tab. The lab has a
**library**: five built-in strategies, the ones that earned their place in a two-year audit of Base
pools (`reports/LIBRARY.md`): the ladder, the ladder as a block, a Bollinger band range, and two
"wait out the move" strategies for volatile pairs (the plain one and one that also watches WETH's
moves; research in `reports/DIRECTIONAL.md`), plus the ones you saved under your own names
(`config/strategies.json`). **Select strategy** lists them all with the APR each would have earned on the
chosen pool and period (backtested four at a time when the list opens), so you can sort by it, open
one, or add it to the comparison. **New**, **Save** and **Save as…** manage your own.

There are two kinds of strategy you can edit:

- **Simple rules** (strategies saved before node strategies could do the same; new ones are node
  strategies): one range of a set width, a trigger band, a delay and a cooldown, cutoffs and exits.
  You can drag the range, trigger, cutoff and exit handles on the chart.
- **Node strategies**: add blocks from the toolbox (or a right-click) and wire them together on a
  canvas you can pan (middle mouse button) and zoom (wheel), with a minimap; a right-click on a block
  renames, duplicates, copies or deletes it. Sources (price, bar
  high/low, fees) feed indicators (SMA, EMA, median, percentile, highest/lowest, standard deviation,
  z-score, Bollinger bands, ATR, Keltner, Donchian, Gaussian channel, RSI, stochastic, stochastic RSI,
  CCI, ADX/DI, MACD, TRIX, parabolic SAR, Ichimoku, Heikin-Ashi, a trend score, a momentum z-score,
  a breakout channel with its side and "hot" window, volatility, and the pool's fee vs arbitrage)
  and maths; conditions (above, below, crosses, between, distance, and/or/not,
  price out of range, range shifted) drive the outputs: the **range** to hold (or, instead, the
  **ladder**: the app's own solve over single ticks, re-solved on its schedule and whenever the
  rebalance condition holds, so the ladder can be combined with exits, holds, parking and the
  optimizer), when to **rebalance**, when to **exit** and when to **hold** still. Widths can be given in ticks, tick spacings or percent,
  and periods in hours. Indicators only use bars that have closed, so a backtest never looks ahead
  (`rs/src/ta.rs` lists the pitfalls it avoids: warm-up, seeding, flat prices, rounding drift).
  Any source or indicator can run on **candles** of 1 minute to a day (built in memory from the stored
  swaps; a value holds from one candle close to the next), so one strategy can mix timeframes. A
  strategy is checked as often as its data can change: every minute when any block works on 1-minute
  candles, else every 5 minutes. The backtest replays it on bars of that length and the worker checks
  such a pool every minute between its regular cycles, so both decide at the same moments.
  **⚡ Optimize** finds the best settings of the blocks you select (Ctrl or Shift + click): pick the
  settings and their ranges, and a search runs many backtests in parallel, spreading over the ranges
  first, then narrowing around the best and polishing one setting at a time (**Select all** ticks every
  setting of one block or of all of them). It is walk-forward: the period it searches is cut into folds
  (4 by default, up to 24), every setting runs on each on its own, and it is scored by its **reliable return**,
  the average yearly return over the folds less 1.64 × their spread ÷ √folds, so a setting that earns
  steadily beats one that made its money in a single stretch. By default the last quarter is held out:
  the search never sees it, the best twelve are run on it and ranked by the same score over all
  periods, and the verdict compares the best with the current settings there; **Apply** writes a
  result into the blocks.
  **✨ Auto** does the picking. Its default check is a **walk-forward of the tuning itself**: the period
  is cut into equal periods (a month each by default, up to 36), and each period from the first tested
  one on is traded with settings tuned only on the periods before it (the last few, or all). Joined up,
  that is what re-tuning every period would have earned with nothing seen in advance; the current
  settings, the strategy without its exits and holding run on the same periods, and the dialog shows
  every period side by side and how far the tuned settings moved between periods. Settings picked on
  the same history (the built-ins were) are in-sample everywhere, so by default the bar is the
  references nothing was tuned on; new settings, tuned on the most recent periods, are suggested only
  when re-tuning beats it over most periods and its edge per period (re-tuning less the bar) is
  reliably above zero, its mean less 1.64 × its spread ÷ √periods: months swing by more than most
  edges, so a higher average alone can be luck. The other check, rotating windows: the last eighth of
  the period is an exam nothing runs on until the end, and the rest is cut into five windows, each
  attempt judged on one of its own (most recent first) and searching on the other four. Attempt 1
  screens every setting of the selected blocks (all when none are) over half to twice its value and
  the candle lengths and tunes the few (at most six) that move the reliable return. When its best
  does not hold up on its window, it tries other ways: wider ranges (dropping settings that changed
  nothing, adding any whose best sat at an edge), small steps on the three that matter most, single
  changes to the blocks (no exit or hold, an optional input left to its default, EMA for SMA, Keltner
  for Bollinger, or for and), and the best change tuned. A winner on its window must then beat the
  current settings on the exam (two candidates at most, the second in each half) to be suggested;
  otherwise it says to keep the current settings, with what each attempt found.
  A **token price** block watches another token: its dollar price from Solidly pools with at least $500
  of active liquidity, against a stable or through a paired token (for example a token/WETH pool times
  WETH/USDC); every qualifying pool is used and the freshest one sets the price. The picker shows which
  tokens can be watched and how much liquidity backs each. Its **stale** output is true while there is
  no reliable price, so it can drive Hold or Exit. Those pools are read from the same local history
  store as everything else, which the worker keeps current.
- **Capital per pool**: every pool runs on its own money: its positions, the tokens it holds idle
  and what it parked. What it earns stays in it, and what it loses is its own loss: nothing is moved
  between pools by itself. Free tokens that no pool holds are **Unallocated** (deposits, withdrawals,
  what a switched-off pool gave back), and pools never touch them. You move money on the Pools tab:
  **Add** (from Unallocated, USDC first), **Withdraw** (tick the pools or All and enter a dollar amount,
  split equally; each pool sells its part to USDC when it next runs, idle tokens first and then the
  same slice of each position, and the USDC lands in Unallocated), **Move** (a withdrawal from one pool
  straight into another) and **Reset to %** (a one-off refill of every pool to a share of the chain).
  Pools run one after another, so what changes in the wallet during a pool's run is that pool's.
  Rewards claimed go to the pools that earned them. On first start each pool keeps what it holds and
  is filled from the free tokens up to its allocation %, as before (`logs/capital.json`, `src/capital.rs`).
- **Parking** (opt-in, Settings › Parking whitelist and per strategy): whatever a pool holds and does
  not use waits in a whitelisted pegged pool (USDC → USDC/USDT, WETH → cbETH/WETH…) until a move needs
  it: out of its pool, the token it waits in (both, after an exit that keeps them as they are, an F/LVR
  gate, say); in its pool, what its ranges left over; and the share of its budget it has not drawn
  from the funding tokens (USDT → USDC/USDT). Always within the pool's budget, never into the pool
  itself. The whitelist is edited by searching the pegged pools and adding a token; each entry has a
  delete button. Every park and unpark step is written to the ledger before it runs, so one cut short
  (a crash, a failed mint, a failed swap back) is finished or undone on the next cycle. When the
  pegged pool runs a strategy of its own here (the USDC/USDT ladder, say), nothing gets a range of
  its own: the tokens are handed to that strategy as a **claim**. Its budget grows by the claim and it
  places them with its own (the ladder over its ticks), the parking pool's budget shrinks by it, and
  an unpark pays the claim back (from the wallet first, the rest withdrawn from the host's positions
  in proportion). The claim is the token amount: on Solidly V3 fees are paid to the wallet through
  reward claims, not into positions, so a pegged host adds or takes nothing measurable on top. In a
  pegged pool with no strategy here, a lot gets a range of its own, which that pool never counts or
  moves; a lot that cannot be valued holds the cycle rather than shrink the budgets. When the pool a
  lot came from is switched off, the lot is handed over to the pool it sits in, if that
  pool runs a strategy: it counts as deployed there and that strategy re-solves on the same cycle,
  whatever its turnover, so the lot is folded into its own ranges; otherwise it stays parked, earning.
  In backtests a parked token earns what its pegged pool paid each day, and nothing before that
  pool's history begins (it parks the day the pool appears); every pool a run depends on (the main
  pool, the pools a token price block reads, the parking pools) is downloaded over its period.
  Each whitelist entry without an ALM of its own picks how its lots are held: a fixed range, or
  re-centred once the price has been outside it for a set time, with its own width. An unpark withdraws
  only what is still on the lot's range, so liquidity removed elsewhere drops the lot instead of
  blocking the strategy. Parked amounts are kept as lots tagged with the pool they came from, apart from that
  pool's own positions, and only those lots come back when the strategy needs them. Backtests count
  parking for strategies with it on whenever the token is whitelisted (the parking pool's fees and
  emissions over its active liquidity plus ours), even while live parking is still switched off.

Every change (saved or not) re-runs a backtest of the period you chose (7 days to two years, or all of a younger pool's history) on the
pool's real swaps, next to up to four strategies you pick from the library and to simply holding, in a
results column that stays in view while you edit. An edit reruns only the strategy being edited; adding
a comparison runs only that one, removing one runs nothing. The results are split into
fees, emissions, swap costs, gas and IL, and node strategies draw their indicators and wanted range on
the price chart. The backtest calls the same decision code the worker runs, so picking the saved
strategy for a pool under **Pools** gives you exactly what you tested. There are no manual overrides:
after an exit a strategy deposits again when its own re-entry rule says so.

After each backtest an **overfit check** runs for the strategy being edited (`rs/src/overfit.rs`): the
same swaps replayed with every setting nudged between half and double (24 neighbours), the period cut
into 8 slices, then walk-forward efficiency, PBO (probability of backtest overfitting, over the 70
half-splits of the slices), stability of the neighbours and consistency across slices, combined into a
0–100 score (70+ robust, below 45 likely overfit). Hover the score for what it means.

### Where the data comes from

Pool events (swaps, deposits, withdrawals) are kept in one local store
(`data/history/<chain>/<pool>/`, one file per day). Everything reads from it: the live decisions,
the profit ledger, fee vs arbitrage loss for every listed pool, and the backtests. It is filled from
the Solidly data processor's public history (`solidly.com/api/history`, compressed in transit), one
download per chain for all of that chain's pools, up to a month of days per request (the processor
limits requests per caller, so long stretches go fastest as ranges). Each later top-up only asks for events
after the last one stored. The minutes the processor has not synced yet come from the chain's RPC,
and once per run a sample of the processor's events is checked against the chain's own logs. Set
`ALM_HISTORY_API=off` to use RPC only.

## Installing

On Windows, download `StableALM-Setup-<version>.exe` from
[Releases](https://github.com/SolidlyLabs/stable-alm/releases) and run it. It installs for your user
only (no administrator rights), adds a Start menu entry, and can start the app when you sign in.
The installer isn't code-signed yet, so Windows SmartScreen may ask you to confirm ("More info", then
"Run anyway"). Uninstalling keeps your data unless you choose to delete it; it never withdraws your
positions.

The same release has the plain `StableALM.exe` if you'd rather not install it, and you can always
build it yourself (below).

## Using the app

1. Start Stable ALM. A tray icon appears: click it for its menu, double-click it to open the dashboard
   (on macOS a click opens the menu; choose Open dashboard).
2. **Pools** lists your pools and every Solidly V3 pool on the chains you show with the chain
   filters at the top (a view filter only: balances, funding and your pools cover every chain). Sort by any column; pegged pairs are shown by default. Add one, enable it,
   give it money with **Add** (from Unallocated, the free tokens no pool holds), and watch a few
   dry-run cycles.
3. When the plan looks right, paste your wallet's private key under **Settings** and press **Go
   live**.
   - On Windows the key is encrypted for your user account (DPAPI); on macOS it goes into the
     Keychain.
   - It never leaves your computer and is never shown again.
   - The same address is used on every chain. Keep a little of each chain's gas token there.

Your settings, data and logs are kept in `%LOCALAPPDATA%\StableALM` (Windows) or
`~/Library/Application Support/StableALM` (macOS). Run only one copy per wallet.

> Volatile pairs (for example WETH/USDC) can be shown after a confirmation, but the strategy has
> **not** been tested on them. On a volatile pair, concentrated liquidity can lose more to price
> moves than it earns in fees.

## Building the app

You need the Rust toolchain ([rustup.rs](https://rustup.rs)). Then:

```bash
git clone https://github.com/SolidlyLabs/stable-alm.git
cd stable-alm/rs
cargo build --release
```

The app is `rs/target/release/StableALM.exe` on Windows, or `rs/target/release/StableALM` on macOS.
It is a single file of about 2.3 MB; copy it anywhere and run it.

To build the Windows installer, install [Inno Setup 6](https://jrsoftware.org/isinfo.php) and run, from `rs/`:

```bash
ISCC /DAppVersion=0.4.0 installer/StableALM.iss     # -> rs/dist/StableALM-Setup-0.4.0.exe
```

Useful command-line options:

```text
StableALM --headless      run the worker and dashboard without a tray icon
StableALM --once          run one cycle in the foreground and print the status
StableALM --quit          stop the running instance after its current cycle
```

## Repository

| Folder | What |
|---|---|
| `rs/` | The desktop app (Rust): worker, dashboard (`rs/static/`), tray, backtester and optimizer. |
| `rs/parity-fixtures/` | What the original Python implementation computed, frozen: `cargo test` checks the app against it. |
| `reports/` | Backtest reports and research write-ups. |
| `docs/` | Design notes (`MULTICHAIN_PLAN.md`). |

Backtests run in the app (Strategy lab), on the history it downloads itself. The app was first
written in Python: that version (backtester, reference executor, data scripts and the research code
behind the older reports) is no longer in the repository. It is in the git history up to tag v0.4.0.
Its results are kept: `cargo test` checks the tick math, calldata, signing, resize plans and ladder
targets against them (`rs/src/parity.rs`).

| Report | Question |
|---|---|
| [`REPORT.md`](reports/REPORT.md) | Which strategy, on which pair, and what it really earns |
| [`IDEAS.md`](reports/IDEAS.md) | Ladder variants that were tried and rejected |
| [`nested_report.md`](reports/nested_report.md) | Overlapping ranges and centred pyramids vs the ladder |
| [`VOLATILE.md`](reports/VOLATILE.md) | Fee capture on volatile pairs, and fee vs arbitrage loss |

Every report except `LIBRARY.md` was made with the Python version (see above).

## License

Apache License 2.0. See [`LICENSE`](LICENSE).
