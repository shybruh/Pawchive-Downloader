//! Site accounts (Settings → Accounts): the "accounts" engine object, in Rust.
//!
//! Lists the sites you can sign in to, saves or clears a session cookie / token in the encrypted
//! vault, signs in with a username and password, reads a sign-in from an installed browser and
//! tests a saved session. Slow work runs on its own thread; its result arrives as a signal
//! carrying the provider id, as before.

mod browser;
mod sites;
pub(crate) mod vault;

use crate::engine::Engine;
use crate::host::native::{Events, NativeObject};
use serde_json::{json, Map, Value};
use std::sync::{Arc, OnceLock};
use vault::Vault;

const OBJ: &str = "accounts";
/// Fields that can be sent to a site to sign in. A saved password is never one of them.
const SESSION_FIELDS: &[&str] = &["cookie", "token", "api_key"];

struct Provider {
    id: &'static str,
    name: &'static str,
    domain: &'static str,
    category: &'static str,
    auth_type: &'static str,
    credentials_login: bool,
    browser_import: bool,
    features: &'static [&'static str],
    help_tip: &'static str,
}

const PROVIDERS: &[Provider] = &[
    Provider {
        id: "cumst",
        name: "Cum.st",
        domain: "cum.st",
        category: "Mirror Platform",
        auth_type: "account",
        credentials_login: true,
        browser_import: true,
        features: &["Favorite Mode account post downloads", "Tag-based subfolder routing", "Access to cdn.cum.st media streams", "Direct storage key downloads"],
        help_tip: "Sign in with your cum.st username or email and password, or on the site.",
    },
    Provider {
        id: "pawchive",
        name: "Pawchive",
        domain: "pawchive.pw",
        category: "Official Mirror",
        auth_type: "account",
        credentials_login: true,
        browser_import: true,
        features: &["Download temporary oversized files (t1.pawchive.pw)", "Tag-based category organization", "High-speed multi-threaded downloads"],
        help_tip: "Sign in with your Pawchive username and password, or on the site.",
    },
    Provider {
        id: "gofile",
        name: "GoFile",
        domain: "gofile.io",
        category: "Cloud Host",
        auth_type: "token",
        credentials_login: false,
        browser_import: false,
        features: &["Download GoFile links (needs a GoFile Premium account)"],
        help_tip: "GoFile only lets apps read folders with a Premium account's API token (gofile.io/myprofile). Without one, open GoFile links in your browser.",
    },
];

fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn str_of(v: &Map<String, Value>, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// The secret a site is signed in with ("" when none). A saved token counts as a cookie.
fn secret_of(entry: &Map<String, Value>) -> String {
    SESSION_FIELDS.iter().map(|k| str_of(entry, k)).find(|s| !s.is_empty()).unwrap_or_default()
}

/// "session=eyJhbG...9a8f": enough to recognise it, never the secret itself
fn mask(val: &str) -> String {
    let val = val.trim();
    if val.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = val.chars().collect();
    if chars.len() <= 12 {
        return "••••••••".into();
    }
    if let Some(inner) = val.strip_prefix("session=") {
        let ic: Vec<char> = inner.chars().collect();
        if ic.len() <= 10 {
            return "session=••••••••".into();
        }
        return format!("session={}...{}", ic[..6].iter().collect::<String>(), ic[ic.len() - 4..].iter().collect::<String>());
    }
    format!("{}...{}", chars[..8].iter().collect::<String>(), chars[chars.len() - 4..].iter().collect::<String>())
}

pub struct Accounts {
    engine: Arc<Engine>,
    events: Events,
    vault: OnceLock<Arc<Vault>>,
}

impl Accounts {
    pub fn new(engine: Arc<Engine>, events: Events) -> Self {
        Self { engine, events, vault: OnceLock::new() }
    }

    /// The vault, in the folder the engine reported (it's up by the time the UI asks).
    fn vault(&self) -> Result<Arc<Vault>, String> {
        if let Some(v) = self.vault.get() {
            return Ok(v.clone());
        }
        let info = self.engine.wait_ready(std::time::Duration::from_secs(60))?;
        if info.data_dir.as_os_str().is_empty() {
            return Err("the engine didn't say where its data is".into());
        }
        Ok(self.vault.get_or_init(|| Arc::new(Vault::new(&info.data_dir, &info.config_dir))).clone())
    }

    fn summary(p: &Provider, entry: &Map<String, Value>) -> Value {
        let secret = secret_of(entry);
        let logged_in = !secret.is_empty();
        let days = entry.get("days_remaining").and_then(Value::as_u64).unwrap_or(0);
        let mut status = if logged_in { "Connected".to_string() } else { "Not Connected".to_string() };
        if logged_in && days > 0 && days < 700 {
            status.push_str(&format!(" ({days}d remaining)"));
        }
        json!({
            "id": p.id, "name": p.name, "domain": p.domain, "icon": "", "category": p.category,
            "auth_type": p.auth_type, "supports_credentials_login": p.credentials_login,
            "is_logged_in": logged_in, "status_text": status,
            "status_color": if logged_in { "#10B981" } else { "#64748B" },
            "username": str_of(entry, "username"), "masked_value": mask(&secret),
            "supports_browser_import": p.browser_import, "supports_test_connection": true,
            "features": p.features, "help_tip": if p.auth_type == "token" { p.help_tip } else { "" },
            "updated_at": entry.get("updated_at").cloned().unwrap_or(json!(0)),
            "is_disabled": false, "disabled_message": "",
        })
    }

    /// Saves fields for a provider (never a password), keeping what else is stored for it.
    fn store(vault: &Vault, id: &str, fields: Vec<(&str, Value)>) -> Result<(), String> {
        vault.update(|providers| {
            let entry = providers.entry(id.to_string()).or_insert_with(|| json!({}));
            let Some(entry) = entry.as_object_mut() else { return };
            entry.remove("password");
            for (k, v) in fields {
                if k == "password" {
                    continue;
                }
                let v = if k == "cookie" {
                    // A bare JWT pasted on its own is the "session" cookie
                    let c = v.as_str().unwrap_or("").trim().to_string();
                    json!(if c.starts_with("eyJ") && !c.contains("session=") { format!("session={c}") } else { c })
                } else {
                    v
                };
                entry.insert(k.to_string(), v);
            }
            entry.insert("updated_at".into(), json!(now()));
            let cookie = str_of(entry, "cookie");
            if !cookie.is_empty() {
                let (days, status) = sites::expiry(&cookie);
                entry.insert("expiry_status".into(), json!(status));
                entry.insert("days_remaining".into(), json!(days));
            }
        })
    }

    /// Runs `work` on its own thread (network and browser files are slow).
    fn background<F: FnOnce(Arc<Vault>, Events) + Send + 'static>(&self, work: F) -> Result<Value, String> {
        let (vault, events) = (self.vault()?, self.events.clone());
        std::thread::spawn(move || work(vault, events));
        Ok(Value::Null)
    }
}

fn arg(args: &[Value], i: usize) -> String {
    args.get(i).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

impl NativeObject for Accounts {
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            ("getProvidersList", vec![]),
            ("saveProviderCredential", vec!["providerId", "credVal"]),
            ("clearProviderCredential", vec!["providerId"]),
            ("validateProviderSession", vec!["providerId"]),
            ("loginProviderWithCredentials", vec!["providerId", "username", "password"]),
            ("importBrowserCookiesToProvider", vec!["providerId", "browserId"]),
        ]
    }

    fn signals(&self) -> Vec<&'static str> {
        vec!["providersChanged", "providerValidationFinished", "providerLoginFinished", "providerImportFinished"]
    }

    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        let id = arg(&args, 0);
        match method {
            "getProvidersList" => {
                let stored = self.vault()?.read()?;
                let empty = Map::new();
                Ok(Value::Array(
                    PROVIDERS
                        .iter()
                        .map(|p| Self::summary(p, stored.get(p.id).and_then(Value::as_object).unwrap_or(&empty)))
                        .collect(),
                ))
            }
            "saveProviderCredential" => {
                let p = provider(&id).ok_or("unknown site")?;
                let key = if p.auth_type == "token" { "token" } else { "cookie" };
                Self::store(&*self.vault()?, &id, vec![(key, json!(arg(&args, 1)))])?;
                self.events.signal(OBJ, "providersChanged", vec![]);
                Ok(json!(true))
            }
            "clearProviderCredential" => {
                self.vault()?.update(|providers| {
                    providers.remove(&id);
                })?;
                self.events.signal(OBJ, "providersChanged", vec![]);
                Ok(json!(true))
            }
            "validateProviderSession" => self.background(move |vault, events| {
                let stored = vault.read().unwrap_or_default();
                let entry = stored.get(&id).and_then(Value::as_object).cloned().unwrap_or_default();
                let secret = secret_of(&entry);
                let (ok, msg, name) = if secret.is_empty() {
                    (false, "Not connected. Sign in or paste a cookie first.".to_string(), String::new())
                } else {
                    let r = sites::validate(&id, &secret, &str_of(&entry, "username"));
                    (r.ok, r.message, r.username)
                };
                if ok {
                    let mut fields = vec![("last_verified", json!(now()))];
                    if !name.is_empty() {
                        fields.push(("username", json!(name)));
                    }
                    let _ = Self::store(&vault, &id, fields);
                }
                events.signal(OBJ, "providerValidationFinished", vec![json!(id), json!(ok), json!(msg)]);
                events.signal(OBJ, "providersChanged", vec![]);
            }),
            "loginProviderWithCredentials" => {
                let (user, pass) = (arg(&args, 1), arg(&args, 2));
                self.background(move |vault, events| {
                    let result = if user.is_empty() || pass.is_empty() {
                        Err("Please enter both username and password.".to_string())
                    } else {
                        sites::login(&id, &user, &pass)
                    };
                    let (ok, msg) = match result {
                        Ok(s) => match Self::store(
                            &vault,
                            &id,
                            vec![("username", json!(s.username)), ("cookie", json!(s.cookie)), ("last_login", json!(now()))],
                        ) {
                            Ok(()) => (true, format!("Signed in as {user}")),
                            Err(e) => (false, e),
                        },
                        Err(e) => (false, e),
                    };
                    events.signal(OBJ, "providerLoginFinished", vec![json!(id), json!(ok), json!(msg)]);
                    if ok {
                        events.signal(OBJ, "providersChanged", vec![]);
                    }
                })
            }
            "importBrowserCookiesToProvider" => self.background(move |vault, events| {
                let domains: Vec<String> = provider(&id).map(|p| vec![p.domain.to_string()]).unwrap_or_default();
                let result = if domains.is_empty() { Err("unknown site".to_string()) } else { browser::import(&domains) };
                let (ok, msg) = match result {
                    Ok(found) => match Self::store(&vault, &id, vec![("cookie", json!(found.cookie))]) {
                        Ok(()) => (true, format!("Imported from {}", found.browser)),
                        Err(e) => (false, e),
                    },
                    Err(e) => (false, e),
                };
                if ok {
                    events.signal(OBJ, "providersChanged", vec![]);
                }
                events.signal(OBJ, "providerImportFinished", vec![json!(id), json!(ok), json!(msg)]);
            }),
            _ => Err(format!("no method {method:?}")),
        }
    }
}
