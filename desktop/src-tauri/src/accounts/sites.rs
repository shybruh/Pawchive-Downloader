//! Talking to the sites: password sign-in, testing a saved session, and reading when a session
//! cookie expires.

use base64::Engine as _;
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, SET_COOKIE};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

fn client(follow_redirects: bool) -> Result<Client, String> {
    Client::builder()
        .user_agent(BROWSER_UA)
        .timeout(Duration::from_secs(15))
        .redirect(if follow_redirects { reqwest::redirect::Policy::limited(5) } else { reqwest::redirect::Policy::none() })
        .build()
        .map_err(|e| e.to_string())
}

/// name=value pairs from a response's Set-Cookie headers
fn set_cookies(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What a password sign-in got: the cookie to save and the account name.
pub struct SignedIn {
    pub cookie: String,
    pub username: String,
}

pub fn login(provider: &str, username: &str, password: &str) -> Result<SignedIn, String> {
    match provider {
        "pawchive" => {
            let resp = client(false)?
                .post("https://pawchive.pw/account/login")
                .header("Referer", "https://pawchive.pw/account/login")
                .header("Origin", "https://pawchive.pw")
                .form(&[("username", username), ("password", password)])
                .send()
                .map_err(|e| format!("Connection failed: {e}"))?;
            let session = set_cookies(resp.headers()).into_iter().find(|(k, _)| k == "session").map(|(_, v)| v);
            let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            let back_to_login = location == "/account/login" || location == "/account/login?location=/artists";
            match session {
                Some(s) if location.contains("logged_in=yes") || !back_to_login => {
                    Ok(SignedIn { cookie: format!("session={s}"), username: username.to_string() })
                }
                _ => Err("Username or password is incorrect.".into()),
            }
        }
        "cumst" => {
            let c = client(true)?;
            let post = |path: &str, body: Value| -> Result<Response, String> {
                c.post(format!("https://cum.st{path}"))
                    .header("Origin", "https://cum.st")
                    .header("Referer", "https://cum.st/auth/signin")
                    .header("Accept", "application/json, text/plain, */*")
                    .json(&body)
                    .send()
                    .map_err(|e| format!("Connection failed: {e}"))
            };
            let by_email = username.contains('@');
            let mut resp = if by_email {
                post("/api/auth/sign-in/email", serde_json::json!({"email": username, "password": password}))?
            } else {
                post("/api/auth/sign-in/username", serde_json::json!({"username": username, "password": password}))?
            };
            if !by_email && matches!(resp.status().as_u16(), 400 | 404) {
                let alt = post("/api/auth/sign-in/email", serde_json::json!({"email": username, "password": password}))?;
                if alt.status().as_u16() == 200 {
                    resp = alt;
                }
            }
            match resp.status().as_u16() {
                200 => {
                    let mut cookie: Vec<String> = set_cookies(resp.headers()).into_iter().map(|(k, v)| format!("{k}={v}")).collect();
                    if cookie.is_empty() {
                        let body: Value = resp.json().unwrap_or_default();
                        let token = body.get("token").or_else(|| body.pointer("/session/token")).and_then(Value::as_str);
                        if let Some(t) = token {
                            cookie.push(format!("better-auth.session_token={t}"));
                        }
                    }
                    Ok(SignedIn { cookie: cookie.join("; "), username: username.to_string() })
                }
                400 | 401 => {
                    let body: Value = resp.json().unwrap_or_default();
                    let msg = body.get("message").or_else(|| body.get("error")).and_then(Value::as_str);
                    Err(msg.unwrap_or("Username or password is incorrect.").to_string())
                }
                403 => Err("DDoS protection is active. Use Sign in on the site, or paste the session cookie.".into()),
                code => Err(format!("cum.st answered HTTP {code}.")),
            }
        }
        _ => Err("This site doesn't support signing in with a password.".into()),
    }
}

/// The result of testing a saved session.
pub struct Checked {
    pub ok: bool,
    pub message: String,
    /// The account name, when the site told us
    pub username: String,
}

fn checked(ok: bool, message: impl Into<String>, username: impl Into<String>) -> Checked {
    Checked { ok, message: message.into(), username: username.into() }
}

pub fn validate(provider: &str, secret: &str, saved_username: &str) -> Checked {
    let run = || -> Result<Checked, String> {
        match provider {
            "pawchive" => {
                let resp = client(false)?
                    .get("https://pawchive.pw/account")
                    .header("Cookie", secret)
                    .send()
                    .map_err(|e| format!("Connection failed: {e}"))?;
                match resp.status().as_u16() {
                    200 => {
                        let mut name = saved_username.to_string();
                        if name.is_empty() {
                            let html = resp.text().unwrap_or_default();
                            if let Some(i) = html.find("account-view__identity") {
                                let after = &html[i..];
                                if let Some(gt) = after.find('>') {
                                    name = after[gt + 1..].trim_start().split(|c: char| c.is_whitespace() || c == '<').next().unwrap_or("").to_string();
                                }
                            }
                        }
                        let msg = if name.is_empty() { "Pawchive session active and verified.".to_string() } else { format!("Pawchive session active ({name})") };
                        Ok(checked(true, msg, name))
                    }
                    302 | 401 | 403 => Ok(checked(false, "Pawchive session expired or invalid. Please sign in again.", "")),
                    code => Ok(checked(false, format!("Pawchive answered HTTP {code}."), "")),
                }
            }
            "cumst" => {
                let c = client(true)?;
                let resp = c
                    .get("https://cum.st/api/auth/get-session")
                    .header("Cookie", secret)
                    .header("Referer", "https://cum.st/")
                    .send()
                    .map_err(|e| format!("Connection failed: {e}"))?;
                if resp.status().as_u16() == 200 {
                    let body: Value = resp.json().unwrap_or_default();
                    if let Some(user) = body.get("user").filter(|u| u.is_object()) {
                        let name = ["name", "email", "username"]
                            .iter()
                            .find_map(|k| user.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
                            .unwrap_or("")
                            .to_string();
                        let msg = if name.is_empty() { "Cum.st session active and verified.".to_string() } else { format!("Cum.st session active ({name})") };
                        return Ok(checked(true, msg, name));
                    }
                }
                let home = c.get("https://cum.st/").header("Cookie", secret).send().map_err(|e| format!("Connection failed: {e}"))?;
                Ok(match home.status().as_u16() {
                    200 => checked(true, "Cum.st connection active.", ""),
                    code => checked(false, format!("Cum.st answered HTTP {code}."), ""),
                })
            }
            "gofile" => {
                let c = client(true)?;
                let auth = format!("Bearer {secret}");
                let id: Value = c
                    .get("https://api.gofile.io/accounts/getid")
                    .header("Authorization", &auth)
                    .send()
                    .and_then(|r| r.json())
                    .unwrap_or_default();
                let Some(acc) = id.pointer("/data/id").and_then(Value::as_str).filter(|_| id.get("status").and_then(Value::as_str) == Some("ok"))
                else {
                    return Ok(checked(false, "GoFile didn't accept this token. Copy it again from gofile.io/myprofile.", ""));
                };
                let info: Value = c
                    .get(format!("https://api.gofile.io/accounts/{acc}"))
                    .header("Authorization", &auth)
                    .send()
                    .and_then(|r| r.json())
                    .unwrap_or_default();
                let tier = info.pointer("/data/tier").and_then(Value::as_str).unwrap_or("").to_lowercase();
                Ok(if tier == "premium" {
                    checked(true, "GoFile Premium token works: GoFile links will download.", "")
                } else {
                    checked(
                        false,
                        format!(
                            "This GoFile account is {}. GoFile only lets apps read folders with a Premium account; open GoFile links in your browser instead.",
                            if tier.is_empty() { "not Premium" } else { tier.as_str() }
                        ),
                        "",
                    )
                })
            }
            _ => Ok(checked(true, "Saved securely in the encrypted vault.", "")),
        }
    };
    run().unwrap_or_else(|e| checked(false, e, ""))
}

/// Days until a session cookie expires (from the expiry inside a JWT "session=" token), and the
/// status word saved with it — the same rules the Python engine used.
pub fn expiry(cookie: &str) -> (u64, &'static str) {
    let jwt_exp = cookie.split(';').map(str::trim).find_map(|part| {
        let token = part.strip_prefix("session=")?.trim();
        if !token.starts_with("eyJ") || token.matches('.').count() < 2 {
            return None;
        }
        let payload = token.split('.').nth(1)?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload.trim_end_matches('=')))
            .ok()?;
        serde_json::from_slice::<Value>(&bytes).ok()?.get("exp")?.as_f64()
    });
    let Some(exp) = jwt_exp.filter(|e| *e > 0.0) else {
        return if cookie.is_empty() { (0, "missing") } else { (30, "active") };
    };
    let delta = exp - now() as f64;
    let days = (delta / 86400.0).max(0.0) as u64;
    if days > 730 {
        (730, "active")
    } else if delta <= 0.0 {
        (0, "expired")
    } else if delta <= 172_800.0 {
        (days, "expiring")
    } else {
        (days, "active")
    }
}
