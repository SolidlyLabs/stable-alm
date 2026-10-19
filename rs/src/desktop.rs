//! Tray app: the worker and web UI in one background process with a tray / menu-bar icon.
//! Single instance; quitting waits for the current cycle.

use crate::worker::Worker;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

static QUIT: AtomicBool = AtomicBool::new(false);

pub fn request_quit_local() {
    QUIT.store(true, Ordering::SeqCst);
}

fn port_file() -> std::path::PathBuf {
    crate::paths::home().join("ui.port")
}

pub fn dashboard_url() -> String {
    let port = std::fs::read_to_string(port_file()).unwrap_or_else(|_| "8080".into());
    format!("http://localhost:{}/?t={}", port.trim(), crate::webui::ui_token())
}

// -- platform glue ----------------------------------------------------------------------------------
#[cfg(windows)]
mod plat {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Registry::*;
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Held for the process lifetime, so two instances never manage the same wallet.
    pub fn single_instance() -> bool {
        unsafe {
            CreateMutexW(std::ptr::null(), 0, w("Local\\StableALM").as_ptr());
            GetLastError() != ERROR_ALREADY_EXISTS
        }
    }

    pub fn open(target: &str) {
        unsafe {
            ShellExecuteW(std::ptr::null_mut(), w("open").as_ptr(), w(target).as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
        }
    }

    pub fn message(text: &str, yes_no: bool) -> bool {
        let flags = if yes_no { MB_YESNO | MB_ICONWARNING } else { MB_ICONINFORMATION };
        unsafe { MessageBoxW(std::ptr::null_mut(), w(text).as_ptr(), w("Stable ALM").as_ptr(), flags | MB_SETFOREGROUND) == IDYES }
    }

    const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    pub fn autostart_enabled() -> bool {
        let mut size = 0u32;
        unsafe { RegGetValueW(HKEY_CURRENT_USER, w(RUN).as_ptr(), w("StableALM").as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) == 0 }
    }
    pub fn set_autostart(on: bool) {
        let exe = std::env::current_exe().map(|p| format!("\"{}\"", p.display())).unwrap_or_default();
        unsafe {
            if on {
                let v = w(&exe);
                RegSetKeyValueW(HKEY_CURRENT_USER, w(RUN).as_ptr(), w("StableALM").as_ptr(), REG_SZ, v.as_ptr() as _, (v.len() * 2) as u32);
            } else {
                RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN).as_ptr(), w("StableALM").as_ptr());
            }
        }
    }

    /// A hidden window that owns the tray menu when the app shows it for a single click (the tray's
    /// own menu-on-click would open on the first click and swallow the second of a double-click).
    pub fn menu_owner() -> isize {
        unsafe {
            CreateWindowExW(0, w("STATIC").as_ptr(), w("").as_ptr(), WS_POPUP, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(),
                            std::ptr::null_mut(), std::ptr::null()) as isize
        }
    }
    /// How long Windows waits for the second click of a double-click.
    pub fn double_click() -> std::time::Duration {
        std::time::Duration::from_millis(unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() } as u64)
    }
    /// After a popup menu closes: lets it close properly the next time it is clicked away from.
    pub fn after_menu(owner: isize) {
        unsafe { PostMessageW(owner as _, WM_NULL, 0, 0) };
    }
    /// The tray's own left-click menu: off, the app shows it itself (above).
    pub const MENU_ON_LEFT: bool = false;

    /// Keep the tray's window messages flowing for `d`.
    pub fn pump(d: std::time::Duration) {
        let t0 = std::time::Instant::now();
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            loop {
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if t0.elapsed() >= d {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        }
    }
    pub const LOGIN_LABEL: &str = "Start with Windows";
}

#[cfg(target_os = "macos")]
mod plat {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
    use std::fs::File;

    static mut LOCK: Option<File> = None;
    pub fn single_instance() -> bool {
        let p = crate::paths::home().join("instance.lock");
        let _ = std::fs::create_dir_all(crate::paths::home());
        match File::create(&p) {
            Ok(f) if f.try_lock().is_ok() => {
                unsafe { LOCK = Some(f) };
                true
            }
            _ => false,
        }
    }
    pub fn open(target: &str) {
        let _ = std::process::Command::new("open").arg(target).spawn();
    }
    pub fn message(text: &str, yes_no: bool) -> bool {
        let buttons = if yes_no { "buttons {\"No\", \"Yes\"} default button \"No\"" } else { "buttons {\"OK\"}" };
        let script = format!("display dialog \"{}\" with title \"Stable ALM\" {buttons}", text.replace('"', "'"));
        std::process::Command::new("osascript").args(["-e", &script]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains("Yes")).unwrap_or(false)
    }
    fn agent() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/LaunchAgents/com.stablealm.app.plist")
    }
    pub fn autostart_enabled() -> bool {
        agent().exists()
    }
    pub fn set_autostart(on: bool) {
        if on {
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
            let plist = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>com.stablealm.app</string><key>ProgramArguments</key><array><string>{exe}</string></array><key>RunAtLoad</key><true/></dict></plist>\n");
            let _ = std::fs::create_dir_all(agent().parent().unwrap());
            let _ = std::fs::write(agent(), plist);
        } else {
            let _ = std::fs::remove_file(agent());
        }
    }
    pub fn init() {
        let mtm = MainThreadMarker::new().expect("main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        unsafe { app.finishLaunching() };
    }
    // a click opens the tray's own menu (the menu bar has no double-click)
    pub fn menu_owner() -> isize {
        0
    }
    pub fn double_click() -> std::time::Duration {
        std::time::Duration::ZERO
    }
    pub fn after_menu(_owner: isize) {}
    pub const MENU_ON_LEFT: bool = true;
    pub fn pump(d: std::time::Duration) {
        let mtm = MainThreadMarker::new().expect("main thread");
        let app = NSApplication::sharedApplication(mtm);
        let until = NSDate::dateWithTimeIntervalSinceNow(d.as_secs_f64());
        unsafe {
            while let Some(ev) = app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&until), NSDefaultRunLoopMode, true) {
                app.sendEvent(&ev);
            }
        }
    }
    pub const LOGIN_LABEL: &str = "Start at login";
}

#[cfg(not(any(windows, target_os = "macos")))]
mod plat {
    pub fn single_instance() -> bool {
        true
    }
    pub fn open(target: &str) {
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
    pub fn message(_t: &str, _y: bool) -> bool {
        false
    }
    pub fn autostart_enabled() -> bool {
        false
    }
    pub fn set_autostart(_on: bool) {}
    pub fn menu_owner() -> isize {
        0
    }
    pub fn double_click() -> std::time::Duration {
        std::time::Duration::ZERO
    }
    pub fn after_menu(_owner: isize) {}
    pub const MENU_ON_LEFT: bool = true;
    pub fn pump(d: std::time::Duration) {
        std::thread::sleep(d)
    }
    pub const LOGIN_LABEL: &str = "Start at login";
}

pub use plat::{message, open, single_instance};

fn icon(colour: &str) -> Icon {
    let (r, g, b) = match colour {
        "green" => (46u8, 160u8, 67u8),
        "amber" => (219, 154, 4),
        "red" => (218, 54, 51),
        _ => (128, 128, 128),
    };
    let n = 32usize;
    let mut px = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f64 - 15.5, y as f64 - 15.5);
            let i = (y * n + x) * 4;
            if dx * dx + dy * dy <= 15.0 * 15.0 {
                let bars = [(8usize, 9usize), (14, 15), (20, 12)];
                let white = bars.iter().any(|(bx, h)| x >= *bx && x < bx + 4 && y < 23 && y >= 23 - h);
                let c = if white { (255, 255, 255) } else { (r, g, b) };
                px[i..i + 4].copy_from_slice(&[c.0, c.1, c.2, 255]);
            }
        }
    }
    Icon::from_rgba(px, n as u32, n as u32).expect("icon")
}

fn state(w: &Worker, thread_alive: bool, quitting: bool) -> (&'static str, String) {
    let st = w.status.lock().unwrap().clone();
    if quitting {
        return ("grey", "Stopping after the current cycle...".into());
    }
    let last = st["last_cycle"].as_u64();
    let ago = last.map(|t| format!("{} min ago", crate::engine::now().saturating_sub(t) / 60)).unwrap_or_else(|| "not yet".into());
    let value = st["account"]["value_usd"].as_f64().or_else(|| st["account"]["total_usd"].as_f64());
    let val = value.map(|v| format!(" | ${}", crate::engine::commas_p(v, 0))).unwrap_or_default();
    let mode = st["mode"].as_str().unwrap_or("dry run").to_string();
    let line = format!("{mode} | {} | last cycle {ago}{val}", st["state"].as_str().unwrap_or("?"));
    let stale = last.map(|t| crate::engine::now().saturating_sub(t) > 1800).unwrap_or(false);
    if !st["last_error"].is_null() || stale || !thread_alive {
        return ("red", line);
    }
    let problems = st["pools"].as_object().map(|p| p.values().any(|v| v["mode"] == "guard" || v["mode"] == "error")).unwrap_or(false);
    if crate::paths::stop_file().exists() || problems {
        return ("amber", line);
    }
    (if mode == "LIVE" { "green" } else { "grey" }, line)
}

pub fn run() -> Result<(), String> {
    if !single_instance() {
        open(&dashboard_url());
        return Ok(());
    }
    crate::paths::seed();
    #[cfg(target_os = "macos")]
    plat::init();
    let worker = Worker::new();
    let _port = crate::webui::serve(worker.clone(), crate::webui::wanted_port(), true)?;
    crate::agent::start(worker.clone());
    crate::update::start();
    let w2 = worker.clone();
    let thread = std::thread::Builder::new().name("worker".into()).spawn(move || w2.loop_forever()).map_err(|e| e.to_string())?;

    let open_i = MenuItem::new("Open dashboard", true, None);
    let status_i = MenuItem::new("Starting...", false, None);
    let run_i = MenuItem::new("Run cycle now", true, None);
    let pause_i = CheckMenuItem::new("Paused", true, crate::paths::stop_file().exists(), None);
    let live_i = CheckMenuItem::new("Live trading", true, false, None);
    let login_i = CheckMenuItem::new(plat::LOGIN_LABEL, true, plat::autostart_enabled(), None);
    let logs_i = MenuItem::new("Open logs folder", true, None);
    let data_i = MenuItem::new("Open data folder", true, None);
    let quit_i = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    let sep = || PredefinedMenuItem::separator();
    menu.append_items(&[&open_i, &status_i, &sep(), &run_i, &pause_i, &live_i, &sep(), &login_i, &logs_i, &data_i, &sep(), &quit_i])
        .map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::new().with_menu(Box::new(menu.clone())).with_tooltip("Stable ALM").with_icon(icon("grey"))
        .with_menu_on_left_click(plat::MENU_ON_LEFT).build().map_err(|e| e.to_string())?;
    // a single click opens the menu, a double-click the dashboard: a click waits out the system's
    // double-click time before its menu shows (Windows; elsewhere the tray shows it on the click)
    let owner = plat::menu_owner();
    let mut single: Option<Instant> = None;
    let mut after_double = false;

    let mut colour = "grey";
    let mut quitting = false;
    let mut last_refresh = Instant::now() - Duration::from_secs(10);
    loop {
        plat::pump(Duration::from_millis(100));
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            match ev {
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } if owner != 0 => {
                    // the release that ends a double-click is not a click of its own
                    if !std::mem::take(&mut after_double) {
                        single = Some(Instant::now());
                    }
                }
                TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => {
                    single = None;
                    after_double = true;
                    open(&dashboard_url());
                }
                _ => {}
            }
        }
        if single.map(|t| t.elapsed() >= plat::double_click()).unwrap_or(false) {
            single = None;
            unsafe { menu.show_context_menu_for_hwnd(owner, None) };
            plat::after_menu(owner);
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = ev.id();
            if id == open_i.id() {
                open(&dashboard_url());
            } else if id == run_i.id() {
                worker.request(false);
            } else if id == pause_i.id() {
                let p = crate::paths::stop_file();
                if p.exists() {
                    let _ = std::fs::remove_file(&p);
                    worker.request(false);
                } else {
                    let _ = std::fs::write(&p, "paused from the tray\n");
                }
            } else if id == live_i.id() {
                let mut s = crate::settings::load_settings();
                if s["execute"].as_bool().unwrap_or(false) {
                    s["execute"] = serde_json::json!(false);
                    let _ = crate::settings::save_settings(&s);
                    worker.request(false);
                } else if !crate::signer::configured() {
                    message("Set the private key in the dashboard (Settings) before going live.", false);
                } else if message(&format!("Go live? The app will sign and send real transactions for {} from the next cycle.", s["wallet"].as_str().unwrap_or("")), true) {
                    s["execute"] = serde_json::json!(true);
                    let _ = crate::settings::save_settings(&s);
                    worker.request(false);
                }
            } else if id == login_i.id() {
                plat::set_autostart(!plat::autostart_enabled());
            } else if id == logs_i.id() {
                open(&crate::paths::logs().display().to_string());
            } else if id == data_i.id() {
                open(&crate::paths::home().display().to_string());
            } else if id == quit_i.id() {
                request_quit_local();
            }
        }
        if QUIT.load(Ordering::SeqCst) && !quitting {
            quitting = true;
            last_refresh = Instant::now() - Duration::from_secs(10);
            crate::info!("desktop", "quit requested: waiting for the current cycle to finish");
            worker.stop();
        }
        if quitting && thread.is_finished() {
            crate::webui::stop();
            crate::info!("desktop", "stopped");
            break;
        }
        if last_refresh.elapsed() > Duration::from_secs(3) {
            last_refresh = Instant::now();
            let (c, line) = state(&worker, !thread.is_finished(), quitting);
            let _ = tray.set_tooltip(Some(format!("Stable ALM - {line}").chars().take(127).collect::<String>()));
            status_i.set_text(line.chars().take(80).collect::<String>());
            pause_i.set_checked(crate::paths::stop_file().exists());
            live_i.set_checked(crate::settings::load_settings()["execute"].as_bool().unwrap_or(false));
            login_i.set_checked(plat::autostart_enabled());
            if c != colour {
                colour = c;
                let _ = tray.set_icon(Some(icon(c)));
            }
        }
    }
    let _ = std::fs::remove_file(port_file());
    Ok(())
}

/// `--quit`: ask the running instance to stop (it finishes the current cycle) and wait.
pub fn quit_running(timeout: Duration) -> i32 {
    let Ok(port) = std::fs::read_to_string(port_file()) else { return 0 };
    let url = format!("http://127.0.0.1:{}/api/control", port.trim());
    let r = crate::rpc::agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set("X-Token", &crate::webui::ui_token())
        .set("Host", &format!("127.0.0.1:{}", port.trim()))
        .send_string(r#"{"action":"quit"}"#);
    if r.is_err() {
        return 0; // nothing listening: not running
    }
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if !port_file().exists() {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    1
}

pub fn headless() -> Result<(), String> {
    crate::paths::seed();
    let worker = Worker::new();
    let _port = crate::webui::serve(worker.clone(), crate::webui::wanted_port(), true)?;
    crate::agent::start(worker.clone());
    crate::update::start();
    let w2 = worker.clone();
    let th = std::thread::spawn(move || w2.loop_forever());
    while !QUIT.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(250));
    }
    worker.stop();
    let _ = th.join();
    let _ = std::fs::remove_file(port_file());
    Ok(())
}

