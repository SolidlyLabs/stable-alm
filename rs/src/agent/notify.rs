//! Alerts and Telegram (plan section 14).
//!
//! Every alert goes to data/agent/alerts.jsonl (the dashboard shows the last ones) and the app log.
//! With Telegram on and linked, severe and normal alerts are also sent to the linked private chat,
//! and a plan waiting for approval is sent with Approve / Reject buttons bound to its hash.
//!
//! Linking: the dashboard makes a 6-digit code valid 10 minutes; the user sends it to the bot in a
//! private chat. That chat and that user are then the only ones the bot listens to; anything else
//! is ignored. Unlinking voids every pending approval. The bot token is kept like the signer key
//! (`signer::save_secret("telegram_bot")`); it is never logged.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const ALERTS: &str = "alerts.jsonl";
const TOKEN: &str = "telegram_bot";
static LINK: Mutex<Option<(String, i64)>> = Mutex::new(None);

pub fn alert(severity: &str, title: &str, body: &str) {
    let r = json!({"ts": super::now(), "severity": severity, "title": title, "body": body.chars().take(1000).collect::<String>()});
    super::append(ALERTS, &r);
    match severity {
        "severe" => crate::warn!("agent", "{title}: {body}"),
        _ => crate::info!("agent", "{title}: {body}"),
    }
    let x = super::settings();
    if super::enabled_in(&x) && linked(&x).is_some() && severity != "info" {
        let text = format!("{}{title}\n{body}", if severity == "severe" { "⚠️ " } else { "" });
        std::thread::spawn(move || {
            if let Err(e) = send(&text, None) {
                crate::warn!("agent", "Telegram: {e}");
            }
        });
    }
}

pub fn recent(n: usize) -> Vec<Value> {
    super::tail(ALERTS, n)
}

fn linked(x: &Value) -> Option<(i64, i64)> {
    let t = &x["telegram"];
    if !t["enabled"].as_bool().unwrap_or(false) {
        return None;
    }
    Some((t["chat_id"].as_i64()?, t["user_id"].as_i64()?))
}

fn token() -> Option<String> {
    crate::signer::load_secret(TOKEN).filter(|t| !t.trim().is_empty())
}

fn api(method: &str, body: &Value) -> Result<Value, String> {
    let t = token().ok_or("no bot token set")?;
    let url = format!("https://api.telegram.org/bot{}/{method}", t.trim());
    // the URL holds the token: errors are reported without it
    let r = crate::rpc::agent().post(&url).timeout(std::time::Duration::from_secs(40)).send_json(body.clone());
    match r {
        Ok(r) => {
            let v: Value = r.into_json().map_err(|e| e.to_string())?;
            if v["ok"].as_bool() == Some(true) { Ok(v["result"].clone()) } else { Err(v["description"].as_str().unwrap_or("error").to_string()) }
        }
        Err(ureq::Error::Status(c, r)) => {
            let v: Value = r.into_json().unwrap_or(Value::Null);
            Err(format!("HTTP {c}: {}", v["description"].as_str().unwrap_or("")))
        }
        Err(_) => Err("Telegram is not reachable".into()),
    }
}

fn send(text: &str, keyboard: Option<Value>) -> Result<(), String> {
    let (chat, _) = linked(&super::settings()).ok_or("Telegram is not linked")?;
    let mut b = json!({"chat_id": chat, "text": text.chars().take(4000).collect::<String>(), "disable_web_page_preview": true});
    if let Some(k) = keyboard {
        b["reply_markup"] = json!({"inline_keyboard": k});
    }
    api("sendMessage", &b).map(|_| ())
}

/// Store the bot token and check it (getMe); returns the bot's user name.
pub fn set_token(t: &str) -> Result<String, String> {
    let t = t.trim();
    if t.is_empty() {
        crate::signer::delete_secret(TOKEN);
        unlink("bot token removed");
        return Ok(String::new());
    }
    if !t.contains(':') || t.len() > 100 || t.chars().any(|c| c.is_whitespace()) {
        return Err("that does not look like a bot token (digits:letters)".into());
    }
    let old = token();
    crate::signer::save_secret(TOKEN, t)?;
    match api("getMe", &json!({})) {
        Ok(me) => Ok(me["username"].as_str().unwrap_or("").to_string()),
        Err(e) => {
            match old {
                Some(o) => { let _ = crate::signer::save_secret(TOKEN, &o); }
                None => crate::signer::delete_secret(TOKEN),
            }
            Err(format!("the token did not work: {e}"))
        }
    }
}

pub fn has_token() -> bool {
    token().is_some()
}

/// A new linking code, valid 10 minutes.
pub fn link_code() -> Result<String, String> {
    if token().is_none() {
        return Err("set the bot token first".into());
    }
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).map_err(|e| e.to_string())?;
    let code = format!("{:06}", u32::from_le_bytes(b) % 1_000_000);
    *LINK.lock().unwrap() = Some((code.clone(), super::now() + 600));
    Ok(code)
}

fn save_link(chat: Option<i64>, user: Option<i64>, name: Option<String>) {
    let mut s = crate::settings::load_settings();
    s["experimental"]["telegram"] = json!({"enabled": chat.is_some(), "chat_id": chat, "user_id": user, "user_name": name});
    let _ = crate::settings::save_settings(&s);
}

pub fn unlink(why: &str) {
    save_link(None, None, None);
    super::plan::void_pending(&format!("Telegram unlinked: {why}"));
    super::journal::record("telegram", json!({"linked": false, "why": why}));
}

/// Send a plan waiting for approval, with buttons bound to its hash.
pub fn send_plan(p: &Value) {
    let id = p["plan_id"].as_str().unwrap_or("");
    let h = &p["plan_hash"].as_str().unwrap_or("")[..16.min(p["plan_hash"].as_str().unwrap_or("").len())];
    let mut lines = vec![format!("Plan {id} waits for approval"), p["summary"].as_str().unwrap_or("").to_string()];
    for (i, c) in p["changes"].as_array().into_iter().flatten().enumerate() {
        lines.push(format!("{}. {} {} {}", i + 1, c["type"].as_str().unwrap_or(""), c["pool"].as_str().unwrap_or(""),
            c["target_usd"].as_f64().or(c["start_usd"].as_f64()).map(|u| format!("${u:.0}")).unwrap_or_default()));
    }
    if let Some(w) = p["approval_reasons"].as_array() {
        lines.push(format!("Needs approval: {}", w.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ")));
    }
    let kb = json!([[{"text": "Approve", "callback_data": format!("a:{id}:{h}")}, {"text": "Reject", "callback_data": format!("r:{id}:{h}")}]]);
    let text = lines.join("\n");
    std::thread::spawn(move || {
        if let Err(e) = send(&text, Some(kb)) {
            crate::warn!("agent", "Telegram: {e}");
        }
    });
}

fn status_text(worker: &crate::worker::Worker) -> String {
    let st = worker.status.lock().unwrap().clone();
    let mut out = vec![format!("Stable ALM: {}", st["state"].as_str().or(st["line"].as_str()).unwrap_or("running"))];
    for (name, p) in st["account"]["pools"].as_object().into_iter().flatten() {
        out.push(format!("{name}: ${:.0}, in range {}", p["deployed_usd"].as_f64().unwrap_or(0.0), p["in_range"]));
    }
    let waiting = super::plan::all().into_iter().filter(|p| p["state"] == "awaiting_approval").count();
    out.push(format!("Plans waiting: {waiting}"));
    out.join("\n")
}

/// Handle one update from getUpdates.
fn handle(u: &Value, worker: &crate::worker::Worker) {
    let x = super::settings();
    if let Some(m) = u.get("message") {
        let chat = m["chat"]["id"].as_i64();
        let user = m["from"]["id"].as_i64();
        let private = m["chat"]["type"] == "private";
        let text = m["text"].as_str().unwrap_or("").trim().to_string();
        // linking
        let code = LINK.lock().unwrap().clone();
        if let Some((c, until)) = code {
            let given = text.trim_start_matches("/start").trim();
            if private && given == c && super::now() < until {
                *LINK.lock().unwrap() = None;
                let name = m["from"]["username"].as_str().map(String::from);
                save_link(chat, user, name.clone());
                super::journal::record("telegram", json!({"linked": true, "user": name}));
                let _ = send("Linked. Alerts and approvals for Stable ALM come here. Commands: /status, /plans, /pause, /resume", None);
                return;
            }
        }
        let Some((lc, lu)) = linked(&x) else { return };
        if chat != Some(lc) || user != Some(lu) {
            return;
        }
        let reply = match text.split_whitespace().next().unwrap_or("") {
            "/status" => status_text(worker),
            "/plans" => {
                let w: Vec<Value> = super::plan::all().into_iter().filter(|p| p["state"] == "awaiting_approval").collect();
                for p in &w {
                    send_plan(p);
                }
                if w.is_empty() { "No plan waits for approval.".into() } else { format!("{} plan(s) sent.", w.len()) }
            }
            "/pause" => {
                let _ = std::fs::write(crate::paths::stop_file(), "paused from Telegram\n");
                super::journal::record("telegram", json!({"command": "pause"}));
                "Paused: no transactions until /resume.".into()
            }
            "/resume" => {
                let _ = std::fs::remove_file(crate::paths::stop_file());
                super::journal::record("telegram", json!({"command": "resume"}));
                worker.request(false);
                "Resumed.".into()
            }
            _ => "Commands: /status, /plans, /pause, /resume".into(),
        };
        let _ = send(&reply, None);
    } else if let Some(q) = u.get("callback_query") {
        let Some((lc, lu)) = linked(&x) else { return };
        let ok_sender = q["from"]["id"].as_i64() == Some(lu) && q["message"]["chat"]["id"].as_i64() == Some(lc);
        let data = q["data"].as_str().unwrap_or("");
        let answer = if !ok_sender {
            "Not allowed".to_string()
        } else {
            let parts: Vec<&str> = data.splitn(3, ':').collect();
            match (parts.first().copied(), parts.get(1).copied(), parts.get(2).copied()) {
                (Some(k @ ("a" | "r")), Some(id), Some(h)) => match super::plan::get(id) {
                    Some(p) if p["plan_hash"].as_str().map(|f| f.starts_with(h)).unwrap_or(false) => {
                        match super::plan::decide(id, p["plan_hash"].as_str().unwrap_or(""), k == "a", "telegram") {
                            Ok(p) => format!("Plan {id}: {}", p["state"].as_str().unwrap_or("")),
                            Err(e) => e,
                        }
                    }
                    Some(_) => "The plan changed since it was sent: not approved".into(),
                    None => "No such plan".into(),
                },
                _ => "Unknown button".into(),
            }
        };
        let _ = api("answerCallbackQuery", &json!({"callback_query_id": q["id"], "text": answer.chars().take(190).collect::<String>()}));
        if ok_sender {
            let _ = send(&answer, None);
            worker.request(false);
        }
    }
}

/// Long-poll Telegram while it is on (or a link code is waiting).
pub fn start(worker: Arc<crate::worker::Worker>) {
    std::thread::Builder::new().name("telegram".into()).spawn(move || {
        let mut offset: i64 = 0;
        let mut announced: std::collections::HashSet<String> = Default::default();
        loop {
            let x = super::settings();
            let linking = LINK.lock().unwrap().as_ref().map(|l| l.1 > super::now()).unwrap_or(false);
            let on = super::enabled_in(&x) && token().is_some() && (linked(&x).is_some() || linking);
            if !on {
                std::thread::sleep(std::time::Duration::from_secs(10));
                continue;
            }
            // a plan newly waiting for approval is sent once
            if linked(&x).is_some() {
                for p in super::plan::all().into_iter().filter(|p| p["state"] == "awaiting_approval") {
                    let id = p["plan_id"].as_str().unwrap_or("").to_string();
                    if announced.insert(id) {
                        send_plan(&p);
                    }
                }
            }
            match api("getUpdates", &json!({"offset": offset, "timeout": 25, "allowed_updates": ["message", "callback_query"]})) {
                Ok(v) => {
                    for u in v.as_array().into_iter().flatten() {
                        offset = offset.max(u["update_id"].as_i64().unwrap_or(0) + 1);
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(u, &worker)));
                    }
                }
                Err(e) => {
                    crate::debug!("agent", "Telegram poll: {e}");
                    std::thread::sleep(std::time::Duration::from_secs(30));
                }
            }
        }
    }).ok();
}
