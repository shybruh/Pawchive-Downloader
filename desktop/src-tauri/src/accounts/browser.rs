//! Reading a site's sign-in from an installed browser.
//!
//! Firefox works. Chrome, Edge and Brave encrypt cookies with "app-bound" keys only the browser
//! itself can use, so for them this reports why it can't, and the in-app sign-in is the way in.
//! Databases are copied to a temporary folder first, so a running browser isn't disturbed.

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const COOKIE_NAMES: &[&str] = &[
    "session", "better-auth.session_token", "better-auth.session_data", "session_token", "cf_clearance", "__cf_bm",
    "session_id", "p_session", "__ddg1_", "__ddg8_", "__ddg9_", "__ddg10_", "a_csrf",
];

pub struct Imported {
    pub cookie: String,
    pub browser: String,
}

struct Browser {
    id: &'static str,
    name: &'static str,
    path: PathBuf,
}

fn browsers() -> Vec<Browser> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from).unwrap_or_default();
    #[cfg(windows)]
    let list = {
        let (roaming, local) = (env("APPDATA"), env("LOCALAPPDATA"));
        vec![
            Browser { id: "firefox", name: "Mozilla Firefox", path: roaming.join("Mozilla").join("Firefox").join("Profiles") },
            Browser { id: "edge", name: "Microsoft Edge", path: local.join("Microsoft").join("Edge").join("User Data") },
            Browser { id: "operagx", name: "Opera GX", path: roaming.join("Opera Software").join("Opera GX Stable") },
            Browser { id: "opera", name: "Opera", path: roaming.join("Opera Software").join("Opera Stable") },
            Browser { id: "brave", name: "Brave Browser", path: local.join("BraveSoftware").join("Brave-Browser").join("User Data") },
            Browser { id: "chrome", name: "Google Chrome", path: local.join("Google").join("Chrome").join("User Data") },
        ]
    };
    #[cfg(not(windows))]
    let list = {
        let home = env("HOME");
        vec![
            Browser { id: "firefox", name: "Mozilla Firefox", path: home.join(".mozilla").join("firefox") },
            Browser { id: "chrome", name: "Google Chrome", path: home.join(".config").join("google-chrome") },
        ]
    };
    list.into_iter().filter(|b| b.path.exists()).collect()
}

fn wanted(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    COOKIE_NAMES.contains(&name) || n.contains("session") || n.contains("clearance")
}

fn clean_value(v: &str) -> bool {
    !v.is_empty() && !v.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7F)
}

/// Copies an SQLite database (with its write-ahead log) somewhere private and opens it.
fn open_copy(db: &Path, tmp: &Path) -> Result<Connection, String> {
    let target = tmp.join("cookies.sqlite");
    std::fs::copy(db, &target).map_err(|e| {
        if e.raw_os_error() == Some(32) {
            "open, close it first".to_string()
        } else {
            e.to_string()
        }
    })?;
    for side in ["-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{side}", db.display()));
        if src.exists() {
            let _ = std::fs::copy(&src, PathBuf::from(format!("{}{side}", target.display())));
        }
    }
    Connection::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|e| e.to_string())
}

fn temp_dir() -> Result<PathBuf, String> {
    let mut n = [0u8; 8];
    getrandom::fill(&mut n).map_err(|e| e.to_string())?;
    let dir = std::env::temp_dir().join(format!("paw_cookie_{}", n.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn host_clause(column: &str, domains: &[String]) -> (String, Vec<String>) {
    let clause = domains.iter().map(|_| format!("{column} LIKE ?")).collect::<Vec<_>>().join(" OR ");
    (clause, domains.iter().map(|d| format!("%{d}")).collect())
}

fn join(cookies: &[(String, String)]) -> String {
    cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
}

// ── Firefox ───────────────────────────────────────────────────────────────────

/// Profile folders, the one Firefox uses by default (profiles.ini) first, then newest first.
fn firefox_dbs(profiles: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![profiles.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "cookies.sqlite") {
                found.push(p);
            }
        }
    }
    let mut defaults = Vec::new();
    for ini in [profiles.join("profiles.ini"), profiles.parent().map(|p| p.join("profiles.ini")).unwrap_or_default()] {
        let Ok(text) = std::fs::read_to_string(&ini) else { continue };
        let base = ini.parent().unwrap_or(Path::new("")).to_path_buf();
        for line in text.lines() {
            if let Some(rel) = line.strip_prefix("Default=").or_else(|| line.strip_prefix("Path=")) {
                defaults.push(base.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)));
            }
        }
    }
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
    found.sort_by_key(|p| {
        let is_default = defaults.iter().any(|d| p.parent().is_some_and(|parent| parent == d));
        (!is_default, std::cmp::Reverse(mtime(p)))
    });
    found
}

fn firefox(profiles: &Path, domains: &[String]) -> Result<Vec<(String, String)>, String> {
    let dbs = firefox_dbs(profiles);
    if dbs.is_empty() {
        return Err("not used".into());
    }
    let (clause, params) = host_clause("host", domains);
    for db in dbs {
        let tmp = temp_dir()?;
        let result = (|| -> Result<Vec<(String, String)>, String> {
            let conn = open_copy(&db, &tmp)?;
            let mut stmt = conn.prepare(&format!("SELECT name, value FROM moz_cookies WHERE {clause}")).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params.iter()), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            let mut out: Vec<(String, String)> = Vec::new();
            for (name, value) in rows.flatten() {
                if wanted(&name) && clean_value(&value) && !out.iter().any(|(n, _)| n == &name) {
                    out.push((name, value));
                }
            }
            Ok(out)
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        if let Ok(found) = result {
            if !found.is_empty() {
                return Ok(found);
            }
        }
    }
    Ok(Vec::new())
}

// ── Chromium browsers ─────────────────────────────────────────────────────────

#[cfg(windows)]
fn chromium(user_data: &Path, domains: &[String]) -> Result<Vec<(String, String)>, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    use base64::Engine as _;

    let mut dbs = Vec::new();
    let mut roots = vec![user_data.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(user_data) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n == "Default" || n.starts_with("Profile ") {
                roots.push(e.path());
            }
        }
    }
    for r in roots {
        for sub in [r.join("Network").join("Cookies"), r.join("Cookies")] {
            if sub.is_file() {
                dbs.push(sub);
                break;
            }
        }
    }
    if dbs.is_empty() {
        return Err("not used".into());
    }
    let state: serde_json::Value = serde_json::from_slice(&std::fs::read(user_data.join("Local State")).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let enc_key = state.pointer("/os_crypt/encrypted_key").and_then(|v| v.as_str()).ok_or("no key")?;
    let enc_key = base64::engine::general_purpose::STANDARD.decode(enc_key).map_err(|e| e.to_string())?;
    let key = super::vault::dpapi::unprotect(enc_key.strip_prefix(b"DPAPI").ok_or("no key")?, None)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;

    let (clause, params) = host_clause("host_key", domains);
    let mut first_error = None;
    for db in dbs {
        let tmp = temp_dir()?;
        let result = (|| -> Result<Vec<(String, String)>, String> {
            let conn = open_copy(&db, &tmp)?;
            let mut stmt =
                conn.prepare(&format!("SELECT name, encrypted_value FROM cookies WHERE {clause}")).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params.iter()), |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))
                .map_err(|e| e.to_string())?;
            let mut out: Vec<(String, String)> = Vec::new();
            for (name, enc) in rows.flatten() {
                if !wanted(&name) || enc.len() < 31 {
                    continue;
                }
                let value = match &enc[..3] {
                    b"v20" => return Err("cookies locked to the browser".into()),
                    b"v10" => cipher.decrypt(Nonce::from_slice(&enc[3..15]), &enc[15..]).ok(),
                    _ => super::vault::dpapi::unprotect(&enc, None).ok(),
                };
                if let Some(v) = value.and_then(|v| String::from_utf8(v).ok()).filter(|v| clean_value(v)) {
                    if !out.iter().any(|(n, _)| n == &name) {
                        out.push((name, v));
                    }
                }
            }
            Ok(out)
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        match result {
            Ok(found) if !found.is_empty() => return Ok(found),
            Ok(_) => {}
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(Vec::new()),
    }
}

#[cfg(not(windows))]
fn chromium(_user_data: &Path, _domains: &[String]) -> Result<Vec<(String, String)>, String> {
    Err("cookies locked to the browser".into())
}

/// The sign-in cookies for `domains` from the first installed browser that has them.
pub fn import(domains: &[String]) -> Result<Imported, String> {
    let list = browsers();
    if list.is_empty() {
        return Err("No supported browsers found on this computer.".into());
    }
    let mut notes = Vec::new();
    for b in list {
        let result = if b.id == "firefox" { firefox(&b.path, domains) } else { chromium(&b.path, domains) };
        match result {
            Ok(found) if !found.is_empty() => return Ok(Imported { cookie: join(&found), browser: b.name.to_string() }),
            Ok(_) => notes.push(format!("{}: not signed in", b.name)),
            Err(e) if e == "not used" => {}
            Err(e) => notes.push(format!("{}: {e}", b.name)),
        }
    }
    let summary = if notes.is_empty() { "none found".to_string() } else { notes.join("; ") };
    Err(format!(
        "No sign-in could be read from your browsers ({summary}). Use Sign in on the site instead, or paste the session cookie."
    ))
}
