//! Stable ALM desktop app: automated liquidity management for Solidly V3.
//!
//!   StableALM                 tray app (worker + dashboard), one instance per user
//!   StableALM --quit          stop the running instance after its current cycle
//!   StableALM --headless      worker + dashboard without a tray (servers, testing)
//!   StableALM --once          one cycle in the foreground, then print the status JSON
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agent;
mod backtest;
mod cache;
mod candles;
mod capital;
mod chain;
mod chains;
mod claim;
mod data;
mod desktop;
mod elasticity;
mod engine;
mod experimental;
mod external;
mod custom;
mod ledger;
mod library;
mod llens;
mod eth;
mod graph;
mod optimize;
mod overfit;
mod parking;
mod ta;
mod history;
mod hyperliquid;
mod ladder;
mod log;
mod market_data;
mod market_flvr;
mod mem;
#[cfg(test)]
mod parity;
mod paths;
mod positions;
mod prices;
mod rpc;
mod settings;
mod signer;
mod solid_lp;
mod store;
mod tickmath;
mod update;
mod onboard;
mod wallet;
mod wallets;
mod webui;
mod worker;

#[global_allocator]
static ALLOC: mem::Counting = mem::Counting;

fn attach_console() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = args.get(1).map(String::as_str).unwrap_or("");
    if !arg.is_empty() {
        attach_console();
    }
    if matches!(arg, "" | "--headless" | "--once") {
        mem::start();
    }
    let code = match arg {
        "" => desktop::run().map(|_| 0).unwrap_or_else(|e| {
            error!("desktop", "{e}");
            desktop::message(&format!("Stable ALM could not start: {e}"), false);
            1
        }),
        "--quit" => desktop::quit_running(std::time::Duration::from_secs(900)),
        "--headless" => desktop::headless().map(|_| 0).unwrap_or_else(|e| {
            eprintln!("{e}");
            1
        }),
        "--once" => {
            paths::seed();
            let w = worker::Worker::new();
            w.cycle();
            println!("{}", serde_json::to_string_pretty(&*w.status.lock().unwrap()).unwrap());
            0
        }
        "--version" => {
            println!("StableALM {}", env!("CARGO_PKG_VERSION"));
            0
        }
        a => {
            eprintln!("unknown option {a}");
            2
        }
    };
    std::process::exit(code);
}
