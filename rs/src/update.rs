//! Updates: the app checks GitHub's latest release and offers it only when it is signed by a key on
//! the signer whitelist built into the app (rs/release/allowed_signers).
//!
//! A release carries SHA256SUMS.txt (the SHA-256 of each file) and SHA256SUMS.txt.sig, an SSH
//! signature of it (`ssh-keygen -Y sign -n stable-alm-release`, rs/release/sign.ps1). An update is
//! offered when the release is newer than this build, the signature verifies with a whitelisted key
//! in that namespace and the file lists the installer; it is installed (Windows, the installed app
//! only) when the downloaded installer's SHA-256 is the signed one. Anything else is refused, with
//! the reason shown. The check runs a minute after start and every 12 hours (settings
//! "update_check"), or on demand.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const NAMESPACE: &str = "stable-alm-release";
const REPO: &str = "SolidlyLabs/stable-alm";
const ALLOWED: &str = include_str!("../release/allowed_signers");
const MAX_SMALL: u64 = 64 << 10;
const MAX_SETUP: u64 = 200 << 20;

static LAST: Mutex<Option<Value>> = Mutex::new(None);
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// The whitelist: (name, key) for each line that is valid for release signatures (a namespaces
/// option, when there is one, must include ours).
pub fn signers(text: &str) -> Vec<(String, ssh_key::PublicKey)> {
    let mut out = vec![];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let t: Vec<&str> = line.split_whitespace().collect();
        let Some(i) = t.iter().position(|x| x.starts_with("ssh-") || x.starts_with("ecdsa-")) else { continue };
        if i == 0 || i + 1 >= t.len() {
            continue;
        }
        let ns_ok = t[1..i].iter().filter_map(|o| o.strip_prefix("namespaces=")).all(|v| v.trim_matches('"').split(',').any(|n| n == NAMESPACE));
        if let (true, Ok(k)) = (ns_ok, ssh_key::PublicKey::from_openssh(&format!("{} {}", t[i], t[i + 1]))) {
            out.push((t[0].to_string(), k));
        }
    }
    out
}

/// Who signed `msg` (the whitelist's name), or why the signature is refused.
pub fn verify(allowed: &str, msg: &[u8], sig: &str) -> Result<String, String> {
    let sig = ssh_key::SshSig::from_pem(sig.trim()).map_err(|e| format!("not an SSH signature: {e}"))?;
    if sig.namespace() != NAMESPACE {
        return Err(format!("signed for {:?}, not for Stable ALM releases", sig.namespace()));
    }
    let (who, key) = signers(allowed).into_iter().find(|(_, k)| k.key_data() == sig.public_key()).ok_or("signed by a key that is not on the signer whitelist")?;
    key.verify(NAMESPACE, msg, &sig).map_err(|e| format!("the signature does not match ({e})"))?;
    Ok(who)
}

fn version(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches('v').split('-').next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Whether release `remote` is newer than `local` (numeric x.y.z; a pre-release suffix is ignored).
pub fn newer(remote: &str, local: &str) -> bool {
    version(remote) > version(local)
}

/// The SHA-256 SHA256SUMS.txt lists for `name` ("<hex>  <name>", or "<hex> *<name>").
pub fn listed(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, n) = l.trim().split_once(char::is_whitespace)?;
        (n.trim().trim_start_matches('*') == name && h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_lowercase())
    })
}

pub fn sha256_hex(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}

fn download(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let r = crate::rpc::agent().get(url).set("Accept", "application/octet-stream").call().map_err(|e| format!("download failed: {e}"))?;
    let mut v = vec![];
    r.into_reader().take(limit + 1).read_to_end(&mut v).map_err(|e| format!("download failed: {e}"))?;
    if v.len() as u64 > limit {
        return Err("download larger than expected".into());
    }
    Ok(v)
}

/// Check GitHub's latest release (blocking: a few small downloads).
pub fn check() -> Result<Value, String> {
    check_from(env!("CARGO_PKG_VERSION"))
}

fn check_from(cur: &str) -> Result<Value, String> {
    let rel = crate::rpc::get_json(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?;
    let tag = rel["tag_name"].as_str().unwrap_or("").to_string();
    let mut out = json!({"current": cur, "latest": tag.trim_start_matches('v'), "url": rel["html_url"], "notes": rel["body"].as_str().unwrap_or("").chars().take(4000).collect::<String>(),
                         "checked": crate::engine::now() as i64, "available": false});
    if newer(&tag, cur) {
        let asset = |f: &dyn Fn(&str) -> bool| rel["assets"].as_array().into_iter().flatten().find(|a| f(a["name"].as_str().unwrap_or(""))).cloned();
        let offer = (|| -> Result<Value, String> {
            let setup = asset(&|n| n.starts_with("StableALM-Setup-") && n.ends_with(".exe")).ok_or("the release has no installer")?;
            let sums = asset(&|n| n == "SHA256SUMS.txt").ok_or("the release has no SHA256SUMS.txt")?;
            let sig = asset(&|n| n == "SHA256SUMS.txt.sig").ok_or("the release is not signed (no SHA256SUMS.txt.sig)")?;
            let url = |a: &Value| a["browser_download_url"].as_str().unwrap_or("").to_string();
            let sums_b = download(&url(&sums), MAX_SMALL)?;
            let sig_b = download(&url(&sig), MAX_SMALL)?;
            let who = verify(ALLOWED, &sums_b, &String::from_utf8_lossy(&sig_b))?;
            let name = setup["name"].as_str().unwrap_or("");
            let hash = listed(&String::from_utf8_lossy(&sums_b), name).ok_or("SHA256SUMS.txt does not list the installer")?;
            Ok(json!({"signed_by": who, "setup": {"name": name, "url": url(&setup), "size": setup["size"], "sha256": hash}}))
        })();
        match offer {
            Ok(o) => {
                out["available"] = json!(true);
                out["signed_by"] = o["signed_by"].clone();
                out["setup"] = o["setup"].clone();
            }
            Err(e) => out["refused"] = json!(e),
        }
    }
    *LAST.lock().unwrap() = Some(out.clone());
    if out["available"] == true {
        crate::info!("update", "version {} is available (signed by {})", out["latest"].as_str().unwrap_or(""), out["signed_by"].as_str().unwrap_or(""));
    } else if let Some(r) = out["refused"].as_str() {
        crate::warn!("update", "version {} is not offered: {r}", out["latest"].as_str().unwrap_or(""));
    }
    Ok(out)
}

/// Why this copy cannot install updates itself (None: it can): only the installed Windows app does.
fn not_installable() -> Option<String> {
    if !cfg!(windows) {
        return Some("updates install on Windows only: download the new version from the release page".into());
    }
    let exe = std::env::current_exe().ok()?;
    let want = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("Programs").join("StableALM");
    let dir = exe.parent()?.to_string_lossy().to_lowercase();
    (dir != want.to_string_lossy().to_lowercase()).then(|| "this copy is not the installed app (a development build): install from the release page".into())
}

/// The last check, and whether this copy can install it.
pub fn status() -> Value {
    let mut s = LAST.lock().unwrap().clone().unwrap_or_else(|| json!({"current": env!("CARGO_PKG_VERSION"), "available": false}));
    s["installable"] = json!(not_installable().is_none());
    s["not_installable"] = json!(not_installable());
    s["installing"] = json!(INSTALLING.load(Ordering::SeqCst));
    s
}

/// Download the offered installer, check it against the signed SHA-256 and start it. The installer
/// stops this app gracefully (after its cycle), replaces it and starts the new version.
pub fn install() -> Result<Value, String> {
    if let Some(why) = not_installable() {
        return Err(why);
    }
    let s = LAST.lock().unwrap().clone().filter(|s| s["available"] == true).ok_or("no signed update to install: check again")?;
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("the update is already being installed".into());
    }
    let r = (|| -> Result<Value, String> {
        let (name, url, want) = (s["setup"]["name"].as_str().unwrap_or(""), s["setup"]["url"].as_str().unwrap_or(""), s["setup"]["sha256"].as_str().unwrap_or(""));
        if name.is_empty() || name.contains(['/', '\\']) || want.len() != 64 {
            return Err("the offered installer is malformed".into());
        }
        let b = download(url, MAX_SETUP)?;
        let got = sha256_hex(&b);
        if got != want {
            return Err(format!("the downloaded installer does not match the signed checksum ({got})"));
        }
        let dir = crate::paths::data().join("updates");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(name);
        std::fs::write(&path, &b).map_err(|e| e.to_string())?;
        // what runs is what was checked
        if std::fs::read(&path).map(|x| sha256_hex(&x)).ok().as_deref() != Some(want) {
            return Err("the saved installer changed on disk".into());
        }
        std::process::Command::new(&path).args(["/SILENT", "/NORESTART"]).spawn().map_err(|e| format!("could not start the installer: {e}"))?;
        crate::info!("update", "installing version {} (signed by {}; installer {got})", s["latest"].as_str().unwrap_or(""), s["signed_by"].as_str().unwrap_or(""));
        Ok(json!({"started": true, "version": s["latest"]}))
    })();
    if r.is_err() {
        INSTALLING.store(false, Ordering::SeqCst);
    }
    r
}

/// The periodic check (settings "update_check", on by default).
pub fn start() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(60));
        loop {
            if crate::settings::load_settings()["update_check"].as_bool().unwrap_or(true) {
                if let Err(e) = check() {
                    crate::warn!("update", "update check failed: {e}");
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(12 * 3600));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &[u8] = include_bytes!("../release/testdata/SHA256SUMS.txt");

    fn allowed(keys: &[&str]) -> String {
        keys.iter().map(|k| {
            let pubk = std::fs::read_to_string(format!("release/testdata/{k}.pub")).unwrap();
            format!("{k} namespaces=\"{NAMESPACE}\" {}\n", pubk.trim())
        }).collect()
    }

    fn sig(k: &str) -> String {
        std::fs::read_to_string(format!("release/testdata/{k}.sig")).unwrap()
    }

    #[test]
    fn a_release_signed_by_a_whitelisted_key_verifies() {
        let list = allowed(&["ed", "rsa"]);
        assert_eq!(verify(&list, SUMS, &sig("ed")), Ok("ed".into()));
        assert_eq!(verify(&list, SUMS, &sig("rsa")), Ok("rsa".into()), "the RSA fallback");
    }

    #[test]
    fn anything_else_is_refused() {
        let list = allowed(&["ed", "rsa"]);
        assert!(verify(&list, SUMS, &sig("other")).unwrap_err().contains("not on the signer whitelist"));
        assert!(verify(&list, SUMS, &sig("wrongns")).unwrap_err().contains("not for Stable ALM releases"));
        let mut changed = SUMS.to_vec();
        changed[0] = b'f';
        assert!(verify(&list, &changed, &sig("ed")).unwrap_err().contains("does not match"));
        assert!(verify(&list, SUMS, "nonsense").is_err());
        // a key listed for another namespace does not count
        let other_ns = std::fs::read_to_string("release/testdata/ed.pub").unwrap();
        assert!(verify(&format!("ed namespaces=\"file\" {}", other_ns.trim()), SUMS, &sig("ed")).is_err());
    }

    #[test]
    fn the_built_in_whitelist_has_the_release_key_and_the_fallback() {
        let s = signers(ALLOWED);
        assert_eq!(s.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["release", "anton"]);
    }

    /// The published release, as an older version would see it (network). cargo test -- --ignored the_published_release --nocapture
    #[test]
    #[ignore]
    fn the_published_release_is_offered_to_older_versions() {
        let r = check_from("0.0.1").expect("GitHub answered");
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
        assert_eq!(r["available"], json!(true), "{}", r["refused"]);
        assert!(r["signed_by"].is_string());
    }

    #[test]
    fn versions_and_checksums() {
        assert!(newer("v0.5.1", "0.5.0") && newer("v0.10.0", "0.9.9") && newer("1.0.0", "0.99.0"));
        assert!(!newer("v0.5.0", "0.5.0") && !newer("v0.4.9", "0.5.0") && !newer("0.5.0-rc1", "0.5.0"));
        let s = String::from_utf8_lossy(SUMS);
        assert_eq!(listed(&s, "StableALM-Setup-9.9.9.exe").as_deref(), Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"));
        assert_eq!(listed(&s, "StableALM.exe"), None);
        assert_eq!(listed("ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789 *x.exe", "x.exe").as_deref(), Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"));
    }
}
