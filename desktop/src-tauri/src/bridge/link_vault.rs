//! "linkVault" and its "creatorsModel": the Link Vault window (was bridge/link_vault_bridge.py).
//!
//! The vault itself is vault.rs (the same database); this is the window's part: the creators for
//! the search and platform, editing and removing, copying and exporting, the check of which links
//! work, and collecting a creator's links (their profile and posts read with the site client).
//! The app tells where the vault is, the site's login and proxy, whether a site is turned off
//! ("vault_context"), copies to the clipboard ("vault_clipboard"), and reads its copy of the vault
//! again after each change ("vault_changed").

use crate::downloader::links;
use crate::downloader::service;
use crate::downloader::sites::clean_text;
use crate::host::native::{Events, NativeObject, Prop};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const NAME: &str = "linkVault";
pub const CREATORS: &str = "linkVault.creatorsModel";

fn log(level: &str, msg: &str) {
    service::log_app(level, msg, "vault", "");
}

/// What only the app knows, for a link (or none)
fn context(url: &str) -> Result<Value, String> {
    let v = service::ask_app("vault_context", json!([url]))?;
    if v["path"].as_str().is_some_and(|p| !p.is_empty()) { Ok(v) } else { Err("the app has no link vault".into()) }
}

/// The passwords typed for a link: a list, or one text split at commas
pub fn password_list(v: &Value) -> Vec<String> {
    let parts: Vec<String> = match v {
        Value::String(s) => s.split(',').map(str::to_string).collect(),
        Value::Array(a) => a.iter().map(|x| match x { Value::String(s) => s.clone(), o => o.to_string() }).collect(),
        _ => Vec::new(),
    };
    parts.iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

#[derive(Default)]
struct State {
    path: String,
    search: String,
    platform: String,
    harvesting: bool,
    status: String,
    rows: Vec<Value>,
    pending: Option<Vec<Value>>,
}

pub struct Vault {
    events: Option<Events>,
    st: Mutex<State>,
    cancel: Arc<AtomicBool>,
}

impl Vault {
    pub fn new(events: Option<Events>) -> Arc<Self> {
        Arc::new(Self { events, st: Mutex::new(State::default()), cancel: Arc::new(AtomicBool::new(false)) })
    }

    fn signal(&self, name: &str, args: Vec<Value>) {
        if let Some(e) = &self.events {
            e.signal(NAME, name, args);
        }
    }

    /// The vault's file (asked from the app once)
    fn path(&self) -> String {
        let known = self.st.lock().unwrap().path.clone();
        if !known.is_empty() {
            return known;
        }
        match context("") {
            Ok(c) => {
                let p = c["path"].as_str().unwrap_or("").to_string();
                self.st.lock().unwrap().path = p.clone();
                p
            }
            Err(_) => String::new(),
        }
    }

    /// One of the vault's operations (vault.rs call); the app's copy follows a change
    fn op(&self, op: &str, args: Value) -> Result<Value, String> {
        let path = self.path();
        if path.is_empty() {
            return Err("the link vault isn't ready yet".into());
        }
        let mut a = args.as_object().cloned().unwrap_or_default();
        a.insert("op".into(), json!(op));
        a.insert("path".into(), json!(path));
        let v = crate::vault::call(0, &Value::Object(a))?;
        if !matches!(op, "tree" | "urls" | "all_passwords" | "data" | "export") {
            self.vault_changed(&v["revision"]);
        }
        Ok(v["result"].clone())
    }

    fn vault_changed(&self, revision: &Value) {
        service::note_app("vault_changed", json!([revision]));
    }

    fn props(&self) -> Value {
        let path = self.st.lock().unwrap().path.clone();
        let (links, creators) = crate::vault::counts(&path).unwrap_or((0, 0));
        let st = self.st.lock().unwrap();
        json!({"totalLinks": links, "totalCreators": creators, "probingActive": crate::vault::probing(),
               "harvestingActive": st.harvesting, "harvestingStatus": st.status})
    }

    fn set_status(&self, status: &str) {
        self.st.lock().unwrap().status = status.to_string();
    }

    /// Tells the window what changed, a few times a second at most
    pub fn start_pump(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::Builder::new().name("vault-events".into()).spawn(move || {
            let mut last = Value::Null;
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if me.st.lock().unwrap().path.is_empty() && me.path().is_empty() {
                    continue;
                }
                let props = me.props();
                if props != last {
                    if let Some(e) = &me.events {
                        let changed: Map<String, Value> = props.as_object().unwrap().iter()
                            .filter(|(k, v)| last.get(k.as_str()) != Some(v)).map(|(k, v)| (k.clone(), v.clone())).collect();
                        e.props(NAME, Value::Object(changed));
                    }
                    last = props;
                }
            }
        }).expect("vault events thread");
    }

    // ── The creators shown ───────────────────────────────────────────────────

    fn tree_async(self: &Arc<Self>, request_id: String) {
        let me = self.clone();
        std::thread::spawn(move || {
            let (search, platform) = { let s = me.st.lock().unwrap(); (s.search.clone(), s.platform.clone()) };
            let tree = match me.op("tree", json!({"search": search, "platform": platform})) {
                Ok(Value::Array(a)) => a,
                Ok(_) => Vec::new(),
                Err(e) => {
                    log("error", &format!("Link vault: building the list failed: {e}"));
                    Vec::new()
                }
            };
            let n = tree.len();
            me.st.lock().unwrap().pending = Some(tree);
            me.signal("asyncResultReady", vec![json!(request_id), json!({"count": n})]);
        });
    }

    fn apply_pending(&self) {
        let n = {
            let mut st = self.st.lock().unwrap();
            match st.pending.take() {
                Some(p) => {
                    st.rows = p;
                    Some(st.rows.len())
                }
                None => None,
            }
        };
        if let (Some(n), Some(e)) = (n, &self.events) {
            e.model(CREATORS, n);
        }
    }

    // ── Checking which links work ────────────────────────────────────────────

    fn probe(self: &Arc<Self>, creator: Option<String>, cancel: Arc<AtomicBool>) {
        let me = self.clone();
        std::thread::spawn(move || {
            let path = me.path();
            if let Ok(v) = crate::vault::probe_links(&path, creator.as_deref(), &cancel, &|_| {}) {
                me.vault_changed(&v["revision"]);
            }
            me.signal("probingFinished", vec![]);
        });
    }

    // ── Collecting a creator's links ─────────────────────────────────────────

    fn harvest(self: &Arc<Self>, url: &str, probe: bool, page_start: usize, page_end: usize) {
        let url = url.trim().to_string();
        if url.is_empty() {
            self.signal("harvestFinished", vec![json!(false), json!(""), json!(0), json!(0)]);
            return;
        }
        {
            let mut st = self.st.lock().unwrap();
            if st.harvesting {
                drop(st);
                log("warning", "The link vault is already collecting a creator's links.");
                return;
            }
            st.harvesting = true;
            st.status = "Connecting...".into();
        }
        self.cancel.store(false, Ordering::Relaxed);
        let me = self.clone();
        std::thread::spawn(move || {
            let (ok, name, new, n) = match me.harvest_run(&url, probe, page_start, page_end) {
                Ok(r) => r,
                Err(e) => {
                    log("error", &format!("Link vault: collecting links failed: {e}"));
                    (false, e, 0, 0)
                }
            };
            {
                let mut st = me.st.lock().unwrap();
                st.harvesting = false;
                st.status = String::new();
            }
            me.signal("harvestFinished", vec![json!(ok), json!(name), json!(new), json!(n)]);
        });
    }

    /// (ok, creator's name or why, new links, posts read)
    fn harvest_run(self: &Arc<Self>, url: &str, probe: bool, page_start: usize, page_end: usize) -> Result<(bool, String, u64, usize), String> {
        let cancel = self.cancel.clone();
        let p = links::parse(url);
        if !p.is_valid {
            log("error", &format!("Link vault: not a creator link: '{url}'"));
            return Ok((false, "Invalid URL".into(), 0, 0));
        }
        let ctx = context(url)?;
        if ctx["refused"].as_bool().unwrap_or(false) {
            return Ok((false, "Site turned off".into(), 0, 0));
        }
        let c = service::site_client(&ctx["site"]);
        let profile = c.creator_profile(&p);
        let raw = profile.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(&p.user_id).to_string();
        let name = { let t = clean_text(&raw); if t.is_empty() { p.user_id.clone() } else { t } };
        let key = format!("{}:{}", p.service, p.user_id).to_lowercase();
        self.set_status(&format!("Scanning posts for {name}..."));
        let quiet = |_: usize, _: usize, _: Option<u64>| {};
        let posts: Vec<Value> = if p.is_single_post {
            c.single_post(&p).into_iter().collect()
        } else if p.is_dms {
            c.user_dms(&p, page_start, page_end, 50, &quiet, &cancel)
        } else {
            c.user_posts(&p, page_start, page_end, 50, &quiet, &cancel, "", "")
        };
        drop(c);
        let n = posts.len();
        if cancel.load(Ordering::Relaxed) {
            log("warning", &format!("Link vault: stopped while reading posts of {name}."));
            return Ok((false, name, 0, n));
        }
        if posts.is_empty() {
            log("info", &format!("Link vault: no posts found for {name}."));
            return Ok((true, name, 0, 0));
        }
        let path = ctx["path"].as_str().unwrap_or("").to_string();
        let a = json!({"posts": posts, "creator_name": name, "domain": p.domain, "service": p.service,
                       "user_id": p.user_id, "cross": true, "site": ctx["site"]});
        let me = self.clone();
        let progress = move |v: Value| {
            if let Some(done) = v.get("done").and_then(Value::as_u64) {
                let total = v.get("total").and_then(Value::as_u64).unwrap_or(n as u64);
                me.set_status(&format!("Processed {done}/{total} posts ({} links found)", v.get("links").and_then(Value::as_u64).unwrap_or(0)));
            }
        };
        let r = crate::vault::collect_posts(&path, &a, &cancel, &progress)?;
        let res = if r.get("result").is_some() { &r["result"] } else { &r };
        if res["cancelled"].as_bool().unwrap_or(false) || cancel.load(Ordering::Relaxed) {
            log("warning", &format!("Link vault: stopped while reading links of {name}."));
            return Ok((false, name, 0, n));
        }
        self.vault_changed(&r["revision"]);
        let new = res["new"].as_u64().unwrap_or(0);
        if probe && new > 0 && !cancel.load(Ordering::Relaxed) {
            self.set_status(&format!("Probing links for {name}..."));
            self.probe(Some(key), cancel.clone());
        }
        log("success", &format!("Link vault: {name} done ({new} new links from {n} posts)."));
        Ok((true, name, new, n))
    }

    fn cancel_harvest(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let mut st = self.st.lock().unwrap();
        st.harvesting = false;
        st.status = "Cancelled".into();
    }

    /// The vault's passwords, each added to the Decompressor's bank
    fn passwords_to_decompressor(&self) -> Result<usize, String> {
        let pws = self.op("all_passwords", json!({}))?;
        let pws: Vec<String> = pws.as_array().into_iter().flatten().map(|p| p.as_str().unwrap_or("").to_string()).collect();
        let dir = context("")?["config_dir"].as_str().unwrap_or("").to_string();
        if !dir.is_empty() {
            for pw in pws.iter().filter(|p| !p.is_empty()) {
                let _ = crate::decompress::bank::call(&json!({"dir": dir, "do": "add", "password": pw, "creator": "Global"}));
            }
            service::note_app("bank_changed", json!([]));
        }
        log("success", &format!("Copied {} password(s) from the link vault to the Decompressor.", pws.len()));
        Ok(pws.len())
    }
}

fn arg(args: &[Value], i: usize) -> String {
    args.get(i).and_then(Value::as_str).unwrap_or("").to_string()
}

fn num(args: &[Value], i: usize, or: usize) -> usize {
    args.get(i).and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64))).map_or(or, |n| n as usize)
}

pub struct Object(pub Arc<Vault>);

impl NativeObject for Object {
    fn props(&self) -> Vec<Prop> {
        vec![
            Prop { name: "totalLinks", kind: "int", writable: false }, Prop { name: "totalCreators", kind: "int", writable: false },
            Prop { name: "probingActive", kind: "bool", writable: false }, Prop { name: "harvestingActive", kind: "bool", writable: false },
            Prop { name: "harvestingStatus", kind: "QString", writable: false },
        ]
    }
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            ("refresh", vec!["search", "platform"]), ("treeAsync", vec!["request_id"]), ("deleteLink", vec!["linkId"]),
            ("updateLinkPasswords", vec!["linkId", "passwords"]), ("deletePost", vec!["postId"]), ("deleteCreator", vec!["creatorKey"]),
            ("cleanDeadLinks", vec![]), ("copyLinks", vec!["creatorKey"]), ("exportToFolder", vec!["folderPath"]),
            ("copyPasswordsToDecompressor", vec![]), ("probeHealth", vec![]),
            ("harvestCreator", vec!["url", "probeHealth", "pageStart", "pageEnd"]), ("cancelHarvest", vec![]),
        ]
    }
    fn signals(&self) -> Vec<&'static str> {
        vec!["changed", "probingFinished", "harvestFinished", "asyncResultReady"]
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        self.0.path();
        self.0.props().get(prop).cloned().ok_or_else(|| format!("unknown property {prop:?}"))
    }
    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        let v = &self.0;
        let ok = |r: Result<Value, String>| r.map(|x| json!(x.as_bool().unwrap_or(false)));
        match method {
            "refresh" => {
                let mut st = v.st.lock().unwrap();
                st.search = arg(&args, 0);
                st.platform = arg(&args, 1);
                Ok(Value::Null)
            }
            "treeAsync" => {
                v.tree_async(arg(&args, 0));
                Ok(Value::Null)
            }
            "deleteLink" => ok(v.op("delete_link", json!({"id": arg(&args, 0)}))),
            "updateLinkPasswords" => {
                let pws = password_list(args.get(1).unwrap_or(&Value::Null));
                ok(v.op("passwords_set", json!({"id": arg(&args, 0), "passwords": pws})))
            }
            "deletePost" => ok(v.op("delete_post", json!({"id": arg(&args, 0)}))),
            "deleteCreator" => ok(v.op("delete_creator", json!({"key": arg(&args, 0)}))),
            "cleanDeadLinks" => v.op("clean_dead", json!({})),
            "copyLinks" => {
                let urls = v.op("urls", json!({"creator": arg(&args, 0)}))?;
                let urls: Vec<&str> = urls.as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                if !urls.is_empty() {
                    service::ask_app("vault_clipboard", json!([urls.join("\n")]))?;
                }
                Ok(json!(urls.len()))
            }
            "exportToFolder" => {
                let mut p = arg(&args, 0);
                if let Some(rest) = p.strip_prefix("file:///") {
                    p = rest.to_string();
                } else if let Some(rest) = p.strip_prefix("file:") {
                    p = rest.to_string();
                }
                v.op("export", json!({"dir": crate::downloader::py::normpath(&p)}))
            }
            "copyPasswordsToDecompressor" => v.passwords_to_decompressor().map(|n| json!(n)),
            "probeHealth" => {
                v.probe(None, Arc::new(AtomicBool::new(false)));
                Ok(Value::Null)
            }
            "harvestCreator" => {
                v.harvest(&arg(&args, 0), args.get(1).and_then(Value::as_bool).unwrap_or(true), num(&args, 2, 1), num(&args, 3, 999_999));
                Ok(Value::Null)
            }
            "cancelHarvest" => {
                v.cancel_harvest();
                Ok(Value::Null)
            }
            m => Err(format!("{NAME} has no method {m:?}")),
        }
    }
}

pub struct Creators(pub Arc<Vault>);

const ROLES: &[&str] = &["creator"];

impl NativeObject for Creators {
    fn props(&self) -> Vec<Prop> {
        vec![Prop { name: "count", kind: "int", writable: false }]
    }
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        vec![("applyPending", vec![]), ("hasKey", vec!["key"]), ("pendingHasKey", vec!["key"])]
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        match prop {
            "count" => Ok(json!(self.0.st.lock().unwrap().rows.len())),
            _ => Err(format!("unknown property {prop:?}")),
        }
    }
    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        let has = |rows: &[Value], k: &str| rows.iter().any(|r| r["creator_key"].as_str() == Some(k));
        match method {
            "applyPending" => {
                self.0.apply_pending();
                Ok(Value::Null)
            }
            "hasKey" => Ok(json!(has(&self.0.st.lock().unwrap().rows, &arg(&args, 0)))),
            "pendingHasKey" => {
                let st = self.0.st.lock().unwrap();
                Ok(json!(has(st.pending.as_deref().unwrap_or(&st.rows[..]), &arg(&args, 0))))
            }
            m => Err(format!("{CREATORS} has no method {m:?}")),
        }
    }
    fn model_roles(&self) -> Option<&'static [&'static str]> {
        Some(ROLES)
    }
    fn rows(&self, start: usize, count: Option<usize>) -> Result<Value, String> {
        let st = self.0.st.lock().unwrap();
        let total = st.rows.len();
        let s = start.min(total);
        let e = count.map_or(total, |c| (s + c).min(total));
        let rows: Vec<Value> = st.rows[s..e].iter().map(|r| json!({"creator": r})).collect();
        Ok(json!({"total": total, "start": s, "rows": rows}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_typed() {
        assert_eq!(password_list(&json!(" a , b,,c ")), ["a", "b", "c"]);
        assert_eq!(password_list(&json!(["x ", "", " y", 5])), ["x", "y", "5"]);
        assert!(password_list(&json!(null)).is_empty());
    }
}
