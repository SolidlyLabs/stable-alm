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

1. Start Stable ALM. A tray icon appears; double-click it to open the dashboard.
2. **Pools** lists your pools and every Solidly V3 pool on the chains you turn on with the chain
   filters at the top. Sort by any column; pegged pairs are shown by default. Add one, give it an
   allocation (a share of what you hold on that chain), and watch a few dry-run cycles.
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
ISCC /DAppVersion=0.2.0 installer/StableALM.iss     # -> rs/dist/StableALM-Setup-0.2.0.exe
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
| `rs/` | The desktop app (Rust): worker, dashboard (`rs/static/`), tray. |
| `alm/` | The Python backtester and the reference executor that the Rust app is checked against. |
| `scripts/` | Data downloads, backtest grids, report builders, the parity exporter. |
| `reports/` | Backtest reports and research write-ups. |
| `research/volatile/` | The volatile-pair fee-capture study. |
| `tests/` | Offline Python tests. |
| `docs/` | Design notes (`MULTICHAIN_PLAN.md`). |

### Backtesting (optional, Python)

```bash
python -m venv .venv
.venv/Scripts/pip install -r requirements.txt     # .venv/bin/pip on macOS/Linux
python -m pytest tests
```

Download pool history into `data/` with `scripts/download_pool_events.py <pool> <fromBlock>` and
`scripts/download_processor.py <pool> <fromBlock>`. Then run a grid and score it:

```bash
python scripts/run_grid.py --pool usdc-usdt --grid full --start 2025-01-17 --out full_usdc-usdt
python scripts/compare_pairs.py
```

Set `ALM_WALLET` to your LP wallet, so your own past positions aren't counted as competing
liquidity. Grids use 2 worker processes by default, and each holds a full replay market in memory.

To check the Rust engine against the Python reference (tick math, calldata, signing, resize plans,
ladder targets):

```bash
python scripts/export_parity.py --data data --out rs/parity
rs/target/release/StableALM --parity rs/parity
```

| Report | Question |
|---|---|
| [`REPORT.md`](reports/REPORT.md) | Which strategy, on which pair, and what it really earns |
| [`IDEAS.md`](reports/IDEAS.md) | Ladder variants that were tried and rejected |
| [`nested_report.md`](reports/nested_report.md) | Overlapping ranges and centred pyramids vs the ladder |
| [`VOLATILE.md`](reports/VOLATILE.md) | Fee capture on volatile pairs, and fee vs arbitrage loss |

## License

Apache License 2.0. See [`LICENSE`](LICENSE).
