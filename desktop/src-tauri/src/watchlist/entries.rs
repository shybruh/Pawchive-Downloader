//! The watchlist's creators and what's done to them: a port of WatchlistManager's operations
//! (core/watchlist_manager.py: adding, favorites, removing, the "downloaded up to" point, marking
//! updates done or ignored, auto-check, download folders), checked against it by
//! tools/golden_watchlist_entries.py. The changed creators are saved to watchlist.db (store.rs).
//!
//! The app's Python side keeps a copy of every creator (the rest of the app still reads and edits
//! them there). Each call brings its changes along ("sync": only the fields that changed) and gets
//! back the creators the operation changed, with all their fields.

use super::store::{self, Store};
use crate::downloader::{naming, py};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// A creator: its row id, its place in the list, and WatchlistEntry's fields
#[derive(Clone, Debug)]
pub struct Rec {
    pub rid: i64,
    pub seq: i64,
    pub f: Map<String, Value>,
}

impl Rec {
    fn s(&self, k: &str) -> String {
        text(self.f.get(k).unwrap_or(&Value::Null))
    }
    fn list(&self, k: &str) -> Vec<Value> {
        self.f.get(k).and_then(Value::as_array).cloned().unwrap_or_default()
    }
    fn strs(&self, k: &str) -> Vec<String> {
        self.list(k).iter().map(text).collect()
    }
    fn truthy(&self, k: &str) -> bool {
        truthy(self.f.get(k).unwrap_or(&Value::Null))
    }
    fn set(&mut self, k: &str, v: Value) {
        self.f.insert(k.to_string(), v);
    }

    /// WatchlistEntry.store_fields: what's saved, apart from the settings and the updates found
    fn store_fields(&self) -> Value {
        let mut dirs = self.strs("download_dirs");
        let dd = self.s("download_dir");
        if !dd.is_empty() && !dirs.contains(&dd) {
            dirs.insert(0, dd.clone());
        }
        let mut d = Map::new();
        for k in ["url", "service", "domain", "user_id", "creator_name", "last_post_id", "last_post_date", "added_at"] {
            d.insert(k.into(), self.f.get(k).cloned().unwrap_or(json!("")));
        }
        d.insert("auto_check".into(), self.f.get("auto_check").cloned().unwrap_or(json!(true)));
        d.insert("download_dir".into(), json!(if dd.is_empty() { dirs.first().cloned().unwrap_or_default() } else { dd }));
        d.insert("download_dirs".into(), json!(dirs));
        d.insert("ignored_post_ids".into(), Value::Array(self.list("ignored_post_ids")));
        d.insert("cutoff_day_ids".into(), Value::Array(self.list("cutoff_day_ids")));
        d.insert("is_favorite".into(), json!(self.truthy("is_favorite")));
        if self.truthy("last_checked_at") {
            d.insert("last_checked_at".into(), self.f["last_checked_at"].clone());
        }
        if self.truthy("custom_folder") {
            d.insert("custom_folder".into(), json!(true));
        }
        Value::Object(d)
    }

    /// The database row: [rid, pos, fields, options, updates found (trimmed)]
    fn row(&self) -> Value {
        let opts = self.f.get("options").filter(|o| o.is_object()).cloned().unwrap_or_else(|| json!({}));
        let posts: Vec<Value> = self.list("cached_new_posts").iter().map(trim_post).collect();
        json!([self.rid, self.seq, self.store_fields(), opts, posts])
    }

    fn key(&self) -> (String, String) {
        (self.s("user_id").trim().to_lowercase(), self.s("service").trim().to_lowercase())
    }
}

/// str(x) of a field (None counts as "")
fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        v => py::pystr(v),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// int(a) <= int(b) for digit strings
fn int_le(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    (a.len(), a) <= (b.len(), b)
}

/// _day: the date part of a date or date-time
fn day(s: &str) -> String {
    match s.split_once('T') {
        Some((d, _)) => d.to_string(),
        None => s.chars().take(10).collect(),
    }
}

/// A post's day (published, else added)
fn post_day(p: &Value) -> String {
    let pick = ["published", "added"].iter().map(|k| p.get(*k).unwrap_or(&Value::Null)).find(|v| truthy(v)).cloned().unwrap_or(json!(""));
    day(&text(&pick))
}

fn id_of(p: &Value) -> String {
    text(p.get("id").unwrap_or(&json!("")))
}

/// trim_post: what's kept of a found post across restarts
pub fn trim_post(p: &Value) -> Value {
    let mut out = Map::new();
    for k in ["id", "title", "published", "added"] {
        match p.get(k) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s.is_empty() => {}
            Some(v) => {
                out.insert(k.into(), v.clone());
            }
        }
    }
    let f = |x: &Value| {
        let mut m = Map::new();
        for k in ["name", "path", "storageKey"] {
            if let Some(v) = x.get(k).filter(|v| truthy(v)) {
                m.insert(k.into(), v.clone());
            }
        }
        Value::Object(m)
    };
    let has = |x: &Value| x.is_object() && (x.get("path").is_some_and(truthy) || x.get("storageKey").is_some_and(truthy));
    if let Some(file) = p.get("file").filter(|x| has(x)) {
        out.insert("file".into(), f(file));
    }
    let atts: Vec<Value> = p.get("attachments").and_then(Value::as_array).into_iter().flatten().filter(|a| has(a)).map(f).collect();
    if !atts.is_empty() {
        out.insert("attachments".into(), Value::Array(atts));
    }
    Value::Object(out)
}

/// _advance_cutoff: moves the "downloaded up to" point forward (never back) and remembers which
/// posts of that day are already downloaded
fn advance_cutoff(e: &mut Rec, post_id: &str, post_date: &str, day_ids: &[String]) {
    let d = day(post_date);
    let mut ids: BTreeSet<String> = day_ids.iter().filter(|i| !i.is_empty()).cloned().collect();
    if !post_id.is_empty() {
        ids.insert(post_id.to_string());
    }
    let cur = day(&e.s("last_post_date"));
    if !d.is_empty() && (cur.is_empty() || d > cur) {
        e.set("last_post_date", json!(d));
        e.set("last_post_id", json!(post_id));
        e.set("cutoff_day_ids", json!(ids.into_iter().collect::<Vec<_>>()));
    } else if !d.is_empty() && d == cur {
        let last = e.s("last_post_id");
        if !post_id.is_empty() && (last.is_empty() || super::post_id_is_newer(post_id, &last)) {
            e.set("last_post_id", json!(post_id));
        }
        let mut all: BTreeSet<String> = e.strs("cutoff_day_ids").into_iter().collect();
        all.extend(ids);
        e.set("cutoff_day_ids", json!(all.into_iter().collect::<Vec<_>>()));
    } else if d.is_empty() && !post_id.is_empty() && e.s("last_post_id").is_empty() {
        e.set("last_post_id", json!(post_id));
    }
}

fn set_posts(e: &mut Rec, posts: Vec<Value>) {
    e.set("new_post_count", json!(posts.len()));
    e.set("cached_new_posts", Value::Array(posts));
}

fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn log(level: &str, message: &str) {
    crate::downloader::service::log_app(level, message, "watchlist", "");
}

fn repr(s: &str) -> String {
    crate::downloader::sites::py_repr(s)
}

fn site_url(domain: &str, service: &str, user_id: &str) -> String {
    if domain.contains("cum.st") || domain.contains("cumst") {
        format!("https://cum.st/creators/{service}/{user_id}")
    } else {
        format!("https://{domain}/{service}/user/{user_id}")
    }
}

/// A new creator, with all of WatchlistEntry's fields
#[allow(clippy::too_many_arguments)]
fn new_fields(url: &str, service: &str, domain: &str, user_id: &str, creator_name: &str, last_post_id: &str, last_post_date: &str, auto_check: bool, download_dir: &str, options: Value, cutoff: Vec<String>, is_favorite: bool) -> Map<String, Value> {
    let v = json!({
        "url": url, "service": service, "domain": domain, "user_id": user_id, "creator_name": creator_name,
        "last_post_id": last_post_id, "last_post_date": last_post_date, "added_at": now_iso(), "auto_check": auto_check,
        "new_post_count": 0, "download_dir": download_dir, "download_dirs": [], "options": options,
        "ignored_post_ids": [], "cached_new_posts": [], "cutoff_day_ids": cutoff, "last_checked_at": 0.0,
        "custom_folder": false, "is_favorite": is_favorite,
    });
    v.as_object().cloned().unwrap()
}

// ── The list ─────────────────────────────────────────────────────────────────

pub struct Entries {
    store: Arc<Store>,
    list: Vec<Rec>,
    next_rid: i64,
    seq_min: i64,
    seq_max: i64,
    /// Changed since the last save (written by the next one)
    unsaved: HashSet<i64>,
    deleted: HashSet<i64>,
    replace_all: bool,
    /// What this call changed (sent back)
    touched: Vec<i64>,
    gone: Vec<i64>,
}

impl Entries {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store, list: Vec::new(), next_rid: 1, seq_min: 0, seq_max: 0, unsaved: HashSet::new(), deleted: HashSet::new(), replace_all: false, touched: Vec::new(), gone: Vec::new() }
    }

    #[allow(dead_code)] // (the tests; the window's list next)
    pub fn list(&self) -> &[Rec] {
        &self.list
    }

    fn pos(&self, user_id: &str, service: &str) -> Option<usize> {
        let k = (user_id.trim().to_lowercase(), service.trim().to_lowercase());
        self.list.iter().position(|e| e.key() == k)
    }

    fn changed(&mut self, i: usize) {
        let rid = self.list[i].rid;
        self.unsaved.insert(rid);
        if !self.touched.contains(&rid) {
            self.touched.push(rid);
        }
    }

    fn add_at_head(&mut self, f: Map<String, Value>) -> usize {
        let rid = self.next_rid;
        self.next_rid += 1;
        self.seq_min -= 1;
        self.list.insert(0, Rec { rid, seq: self.seq_min, f });
        self.changed(0);
        0
    }

    /// The Python side's changes: rows of {rid, seq, f: changed fields (all fields for a new
    /// creator), w: to be saved}, the creators taken out; `reset`: these rows are the whole list
    pub fn sync(&mut self, s: &Value) {
        if s["reset"].as_bool().unwrap_or(false) {
            *self = Self::new(self.store.clone());
        }
        for r in s["rows"].as_array().into_iter().flatten() {
            let Some(rid) = r["rid"].as_i64() else { continue };
            let seq = r["seq"].as_i64();
            let f = r["f"].as_object().cloned().unwrap_or_default();
            match self.list.iter_mut().find(|e| e.rid == rid) {
                Some(e) => {
                    e.f.extend(f);
                    if let Some(seq) = seq {
                        e.seq = seq;
                    }
                }
                None => self.list.push(Rec { rid, seq: seq.unwrap_or(0), f }),
            }
            if r["w"].as_bool().unwrap_or(false) {
                self.unsaved.insert(rid);
            }
        }
        for rid in s["deleted"].as_array().into_iter().flatten().filter_map(Value::as_i64) {
            if let Some(i) = self.list.iter().position(|e| e.rid == rid) {
                self.list.remove(i);
            }
            self.unsaved.remove(&rid);
            self.deleted.insert(rid);
        }
        if s["replace_all"].as_bool().unwrap_or(false) {
            self.replace_all = true;
        }
        if s["order"].is_array() {
            // (the Python list's order)
            let order: HashMap<i64, usize> = s["order"].as_array().unwrap().iter().filter_map(Value::as_i64).enumerate().map(|(i, r)| (r, i)).collect();
            self.list.sort_by_key(|e| order.get(&e.rid).copied().unwrap_or(usize::MAX));
        }
        for k in ["next_rid", "seq_min", "seq_max"] {
            if let Some(v) = s[k].as_i64() {
                match k {
                    "next_rid" => self.next_rid = self.next_rid.max(v),
                    "seq_min" => self.seq_min = self.seq_min.min(v),
                    _ => self.seq_max = self.seq_max.max(v),
                }
            }
        }
        for e in &self.list {
            self.next_rid = self.next_rid.max(e.rid + 1);
            self.seq_min = self.seq_min.min(e.seq);
            self.seq_max = self.seq_max.max(e.seq);
        }
    }

    /// Writes the creators changed since the last save. False when the database couldn't be
    /// written (they're written with the next save).
    pub fn save(&mut self) -> bool {
        if self.unsaved.is_empty() && self.deleted.is_empty() && !self.replace_all {
            return true;
        }
        let rows: Vec<Value> = if self.replace_all { self.list.iter().map(Rec::row).collect() } else { self.list.iter().filter(|e| self.unsaved.contains(&e.rid)).map(Rec::row).collect() };
        let live: HashSet<i64> = self.list.iter().map(|e| e.rid).collect();
        let deleted: Vec<i64> = self.deleted.iter().filter(|r| !live.contains(r)).copied().collect();
        match self.store.write(&rows, &deleted, self.replace_all) {
            Ok(_) => {
                self.unsaved.clear();
                self.deleted.clear();
                self.replace_all = false;
                true
            }
            Err(e) => {
                log("ERROR", &format!("Failed to save watchlist: {e}"));
                false
            }
        }
    }

    // ── The operations (WatchlistManager's) ──────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub fn add_entry(&mut self, url: &str, creator_name: &str, user_id: &str, service: &str, domain: &str, last_post_id: &str, last_post_date: &str, auto_check: bool, download_dir: &str, options: &Value, is_favorite: bool, include_service: bool) -> bool {
        if let Some(i) = self.pos(user_id, service) {
            let e = &mut self.list[i];
            if !last_post_date.is_empty() || !last_post_id.is_empty() {
                advance_cutoff(e, last_post_id, last_post_date, &[]);
            }
            if !creator_name.is_empty() && creator_name != user_id {
                let old_name = e.s("creator_name");
                let old_was_numeric = old_name == user_id || old_name.is_empty();
                e.set("creator_name", json!(creator_name));
                let dd = e.s("download_dir");
                if !dd.is_empty() && old_was_numeric {
                    let clean_target = py::normpath(&dd);
                    let base = py::basename(&clean_target).to_string();
                    if naming::is_creator_folder(&base, user_id, service, include_service) {
                        let parent = py::dirname(&clean_target);
                        let clean_c = naming::clean_filesystem_text(creator_name, 80, "creator");
                        let new_dir = py::join(&[&parent, &naming::creator_folder(&clean_c, service, include_service)]);
                        if std::path::Path::new(&dd).exists() && !std::path::Path::new(&new_dir).exists() {
                            match std::fs::rename(&dd, &new_dir) {
                                Ok(_) => log("INFO", &format!("Auto-migrated folder '{base}' -> '{}'", py::basename(&new_dir))),
                                Err(err) => log("DEBUG", &format!("Could not rename folder on disk: {err}")),
                            }
                        }
                        e.set("download_dir", json!(new_dir));
                    }
                } else if !download_dir.is_empty() && dd.is_empty() {
                    e.set("download_dir", json!(download_dir));
                }
            } else if !download_dir.is_empty() && e.s("download_dir").is_empty() {
                e.set("download_dir", json!(download_dir));
            }
            if !url.is_empty() {
                e.set("url", json!(url));
            }
            if truthy(options) {
                e.set("options", options.clone());
            }
            if is_favorite {
                e.set("is_favorite", json!(true));
            }
            self.changed(i);
            false
        } else {
            let cutoff = if last_post_id.is_empty() { vec![] } else { vec![last_post_id.to_string()] };
            let opts = if options.is_object() { options.clone() } else { json!({}) };
            self.add_at_head(new_fields(url, service, domain, user_id, creator_name, last_post_id, last_post_date, auto_check, download_dir, opts, cutoff, is_favorite));
            log("INFO", &format!("Added to watchlist: {} [{service}] (last post: {})", repr(creator_name), if last_post_date.is_empty() { "unknown" } else { last_post_date }));
            true
        }
    }

    pub fn set_favorite(&mut self, user_id: &str, service: &str, fav: bool) -> bool {
        let Some(i) = self.pos(user_id, service) else { return false };
        if self.list[i].truthy("is_favorite") != fav {
            self.list[i].set("is_favorite", json!(fav));
            self.changed(i);
        }
        true
    }

    /// The creator's row id
    pub fn add_or_update_favorite(&mut self, user_id: &str, service: &str, creator_name: &str, domain: &str, url: &str, fav: bool) -> i64 {
        if let Some(i) = self.pos(user_id, service) {
            let e = &mut self.list[i];
            e.set("is_favorite", json!(fav));
            if !domain.is_empty() && (domain.contains("cum.st") || domain.contains("cumst")) {
                e.set("domain", json!("cum.st"));
            }
            let name = e.s("creator_name");
            if !creator_name.is_empty() && (name == user_id || name.is_empty()) {
                e.set("creator_name", json!(creator_name));
            }
            if !url.is_empty() {
                e.set("url", json!(url));
            } else if e.s("url").is_empty() {
                let u = site_url(&e.s("domain"), service, user_id);
                e.set("url", json!(u));
            }
            self.changed(i);
            self.list[i].rid
        } else {
            let url = if url.is_empty() { site_url(domain, service, user_id) } else { url.to_string() };
            let name = if creator_name.is_empty() { user_id } else { creator_name };
            let i = self.add_at_head(new_fields(&url, service, domain, user_id, name, "", "", true, "", json!({}), vec![], fav));
            log("INFO", &format!("Added favorite creator to watchlist: {} [{service}]", repr(creator_name)));
            self.list[i].rid
        }
    }

    pub fn remove_entry(&mut self, user_id: &str, service: &str) -> bool {
        let Some(i) = self.pos(user_id, service) else { return false };
        let e = self.list.remove(i);
        self.unsaved.remove(&e.rid);
        self.deleted.insert(e.rid);
        self.gone.push(e.rid);
        log("INFO", &format!("Removed from watchlist: {} [{service}]", repr(&e.s("creator_name"))));
        true
    }

    pub fn update_last_download(&mut self, user_id: &str, service: &str, post_id: &str, post_date: &str, day_ids: &[String]) {
        let Some(i) = self.pos(user_id, service) else { return };
        let e = &mut self.list[i];
        advance_cutoff(e, post_id, post_date, day_ids);
        let cutoff = day(&e.s("last_post_date"));
        let done: HashSet<String> = e.strs("cutoff_day_ids").into_iter().collect();
        let last = e.s("last_post_id");
        let keep: Vec<Value> = e
            .list("cached_new_posts")
            .into_iter()
            .filter(|p| {
                let (d, id) = (post_day(p), id_of(p));
                d > cutoff || (d == cutoff && !done.contains(&id) && !(is_digits(&id) && is_digits(&last) && int_le(&id, &last)))
            })
            .collect();
        set_posts(e, keep);
        self.changed(i);
    }

    pub fn resolve_posts(&mut self, user_id: &str, service: &str, post_ids: Option<&[String]>, latest_id: &str, latest_date: &str, day_ids: &[String]) -> bool {
        let Some(i) = self.pos(user_id, service) else { return false };
        let e = &mut self.list[i];
        match post_ids.filter(|p| !p.is_empty()) {
            Some(ids) => {
                let set: HashSet<&String> = ids.iter().collect();
                let keep: Vec<Value> = e.list("cached_new_posts").into_iter().filter(|p| !set.contains(&id_of(p))).collect();
                let empty = keep.is_empty();
                set_posts(e, keep);
                if empty && (!latest_date.is_empty() || !latest_id.is_empty()) {
                    advance_cutoff(e, latest_id, latest_date, day_ids);
                }
            }
            None => {
                if !latest_date.is_empty() || !latest_id.is_empty() {
                    advance_cutoff(e, latest_id, latest_date, day_ids);
                }
                set_posts(e, vec![]);
            }
        }
        self.changed(i);
        true
    }

    pub fn set_auto_check(&mut self, user_id: &str, service: &str, enabled: bool) {
        if let Some(i) = self.pos(user_id, service) {
            self.list[i].set("auto_check", json!(enabled));
            self.changed(i);
        }
    }

    pub fn set_all_auto_check(&mut self, enabled: bool) -> usize {
        let mut n = 0;
        for i in 0..self.list.len() {
            let cur = self.list[i].f.get("auto_check").map(truthy).unwrap_or(true);
            if cur != enabled {
                self.list[i].set("auto_check", json!(enabled));
                self.changed(i);
                n += 1;
            }
        }
        n
    }

    pub fn set_download_dir(&mut self, user_id: &str, service: &str, dir: &str) -> bool {
        let Some(i) = self.pos(user_id, service) else { return false };
        let e = &mut self.list[i];
        let norm = if dir.is_empty() { String::new() } else { py::normpath(dir) };
        e.set("download_dir", json!(norm));
        if !norm.is_empty() {
            let mut dirs = e.strs("download_dirs");
            if !dirs.contains(&norm) {
                dirs.insert(0, norm);
            }
            e.set("download_dirs", json!(dirs));
        }
        self.changed(i);
        true
    }

    pub fn add_download_dir(&mut self, user_id: &str, service: &str, dir: &str) -> bool {
        let Some(i) = self.pos(user_id, service).filter(|_| !dir.is_empty()) else { return false };
        let e = &mut self.list[i];
        let norm = py::normpath(dir);
        let mut dirs = if e.f.get("download_dirs").is_some_and(Value::is_array) {
            e.strs("download_dirs")
        } else {
            let dd = e.s("download_dir");
            if dd.is_empty() { vec![] } else { vec![dd] }
        };
        if !dirs.contains(&norm) {
            dirs.push(norm.clone());
        }
        e.set("download_dirs", json!(dirs));
        if e.s("download_dir").is_empty() {
            e.set("download_dir", json!(norm));
        }
        self.changed(i);
        true
    }

    pub fn remove_download_dir(&mut self, user_id: &str, service: &str, dir: &str) -> bool {
        let Some(i) = self.pos(user_id, service).filter(|_| !dir.is_empty()) else { return false };
        let e = &mut self.list[i];
        let norm = py::normpath(dir).to_lowercase();
        let dirs: Vec<String> = e.strs("download_dirs").into_iter().filter(|d| py::normpath(d).to_lowercase() != norm).collect();
        e.set("download_dirs", json!(dirs));
        let dd = e.s("download_dir");
        if !dd.is_empty() && py::normpath(&dd).to_lowercase() == norm {
            e.set("download_dir", json!(dirs.first().cloned().unwrap_or_default()));
        }
        self.changed(i);
        true
    }

    /// (`date`: already normalized, "" clears it)
    pub fn set_last_post_date(&mut self, user_id: &str, service: &str, date: &str) -> bool {
        let Some(i) = self.pos(user_id, service) else { return false };
        let e = &mut self.list[i];
        e.set("last_post_date", json!(date));
        e.set("last_post_id", json!(""));
        e.set("cutoff_day_ids", json!([]));
        set_posts(e, vec![]);
        let name = e.s("creator_name");
        self.changed(i);
        log("INFO", &format!("Watchlist: updated last download date for {} [{service}] to {}.", repr(&name), if date.is_empty() { "Never" } else { date }));
        true
    }

    pub fn ignore_post(&mut self, user_id: &str, service: &str, post: &str) -> bool {
        let pid = post.trim().to_string();
        if pid.is_empty() {
            return false;
        }
        let Some(i) = self.pos(user_id, service) else { return false };
        let e = &mut self.list[i];
        let mut ign = e.list("ignored_post_ids");
        if !ign.iter().any(|x| text(x) == pid) {
            ign.push(json!(pid));
        }
        e.set("ignored_post_ids", Value::Array(ign));
        let keep: Vec<Value> = e.list("cached_new_posts").into_iter().filter(|p| py::pystr(p.get("id").unwrap_or(&Value::Null)) != pid).collect();
        set_posts(e, keep);
        self.changed(i);
        true
    }

    pub fn unignore_post(&mut self, user_id: &str, service: &str, post: &str) -> bool {
        let pid = post.trim();
        let Some(i) = self.pos(user_id, service) else { return false };
        let mut ign = self.list[i].list("ignored_post_ids");
        let Some(j) = ign.iter().position(|x| text(x) == pid) else { return false };
        ign.remove(j);
        self.list[i].set("ignored_post_ids", Value::Array(ign));
        self.changed(i);
        true
    }

    pub fn unignore_all(&mut self, user_id: &str, service: &str) -> bool {
        let Some(i) = self.pos(user_id, service) else { return false };
        if !self.list[i].truthy("ignored_post_ids") {
            return false;
        }
        self.list[i].set("ignored_post_ids", json!([]));
        self.changed(i);
        true
    }

    /// The call's answer: what it changed
    fn changes(&mut self) -> Value {
        let rows: Vec<Value> = std::mem::take(&mut self.touched)
            .into_iter()
            .filter_map(|rid| self.list.iter().find(|e| e.rid == rid))
            .map(|e| json!({"rid": e.rid, "seq": e.seq, "f": e.f}))
            .collect();
        json!({"rows": rows, "deleted": std::mem::take(&mut self.gone)})
    }
}

/// The open watchlists, by database path
fn lists() -> &'static Mutex<HashMap<String, Entries>> {
    static L: OnceLock<Mutex<HashMap<String, Entries>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// The app's requests ({"op", "path", "sync", ...}: the engine service's "watchlist" call).
/// Err "not loaded" when this engine hasn't been given the list yet (the app then sends all of it).
pub fn call(a: &Value) -> Result<Value, String> {
    let path = a["path"].as_str().ok_or("no path")?;
    let mut all = lists().lock().unwrap_or_else(|e| e.into_inner());
    let sync = &a["sync"];
    if !all.contains_key(path) {
        if !sync["reset"].as_bool().unwrap_or(false) {
            return Err("watchlist not loaded".into());
        }
        all.insert(path.to_string(), Entries::new(store::store(path)));
    }
    let w = all.get_mut(path).unwrap();
    w.touched.clear();
    w.gone.clear();
    if sync.is_object() {
        w.sync(sync);
    }
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let b = |k: &str, d: bool| a[k].as_bool().unwrap_or(d);
    let ids = |k: &str| -> Vec<String> { a[k].as_array().into_iter().flatten().map(text).collect() };
    let (uid, svc) = (s("user_id"), s("service"));
    let result = match a["op"].as_str().unwrap_or("") {
        "sync" => Value::Null,
        "add_entry" => json!(w.add_entry(&s("url"), &s("creator_name"), &uid, &svc, &s("domain"), &s("last_post_id"), &s("last_post_date"), b("auto_check", true), &s("download_dir"), &a["options"], b("is_favorite", false), b("include_service", true))),
        "set_favorite" => json!(w.set_favorite(&uid, &svc, b("is_favorite", false))),
        "add_or_update_favorite" => json!(w.add_or_update_favorite(&uid, &svc, &s("creator_name"), &s("domain"), &s("url"), b("is_favorite", true))),
        "remove_entry" => json!(w.remove_entry(&uid, &svc)),
        "update_last_download" => {
            w.update_last_download(&uid, &svc, &s("post_id"), &s("post_date"), &ids("day_ids"));
            Value::Null
        }
        "resolve_posts" => {
            let p = a["post_ids"].is_array().then(|| ids("post_ids"));
            json!(w.resolve_posts(&uid, &svc, p.as_deref(), &s("latest_post_id"), &s("latest_post_date"), &ids("day_ids")))
        }
        "set_auto_check" => {
            w.set_auto_check(&uid, &svc, b("enabled", true));
            Value::Null
        }
        "set_all_auto_check" => json!(w.set_all_auto_check(b("enabled", true))),
        "set_download_dir" => json!(w.set_download_dir(&uid, &svc, &s("download_dir"))),
        "add_download_dir" => json!(w.add_download_dir(&uid, &svc, &s("download_dir"))),
        "remove_download_dir" => json!(w.remove_download_dir(&uid, &svc, &s("download_dir"))),
        "set_last_post_date" => json!(w.set_last_post_date(&uid, &svc, &s("date"))),
        "ignore_post" => json!(w.ignore_post(&uid, &svc, &s("post_id"))),
        "unignore_post" => json!(w.unignore_post(&uid, &svc, &s("post_id"))),
        "unignore_all" => json!(w.unignore_all(&uid, &svc)),
        op => return Err(format!("unknown watchlist op {op:?}")),
    };
    let saved = if b("save", true) { w.save() } else { true };
    let mut out = w.changes();
    out["result"] = result;
    out["saved"] = json!(saved);
    out["next_rid"] = json!(w.next_rid);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(v: &Value, from: &str, to: &str) -> Value {
        match v {
            Value::String(s) => json!(s.replace(from, to)),
            Value::Array(a) => Value::Array(a.iter().map(|x| sub(x, from, to)).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), sub(x, from, to))).collect()),
            v => v.clone(),
        }
    }

    fn now_out(mut f: Value) -> Value {
        if f["added_at"].as_str().is_some_and(|a| a >= "2026-01-01") {
            f["added_at"] = json!("<now>");
        }
        f
    }

    /// tools/golden_watchlist_entries.py's operations give the same creators, answers, folders
    /// and database as the Python manager
    #[test]
    fn same_as_python() {
        let fx: Value = serde_json::from_str(include_str!("../../tests/fixtures/watchlist_entries.json")).unwrap();
        let base = std::env::current_dir().unwrap().join("target").join("watchlist_entries_rs");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("222 [fanbox]")).unwrap();
        let b = base.to_str().unwrap().to_string();
        let db = format!("{b}\\config\\watchlist.db");
        let rows: Vec<Value> = fx["start"].as_array().unwrap().iter().map(|e| json!({"rid": e["rid"], "seq": e["seq"], "f": sub(&e["f"], "{BASE}", &b), "w": true})).collect();
        let order: Vec<Value> = fx["start"].as_array().unwrap().iter().map(|e| e["rid"].clone()).collect();
        call(&json!({"op": "sync", "path": db, "sync": {"reset": true, "rows": rows, "order": order, "replace_all": true}})).unwrap();
        for (n, st) in fx["steps"].as_array().unwrap().iter().enumerate() {
            let op = st["op"].as_str().unwrap();
            let mut a = sub(&st["args"], "{BASE}", &b);
            let mut rop = op.to_string();
            match op {
                "add_download_dir" => a["download_dir"] = a["new_dir"].clone(),
                "remove_download_dir" => a["download_dir"] = a["target_dir"].clone(),
                "add_or_update_favorite_creator" => {
                    rop = "add_or_update_favorite".into();
                    if a.get("domain").is_none() {
                        a["domain"] = json!("pawchive.pw");
                    }
                }
                "set_last_post_date" => a["date"] = st["result"][1].clone(),
                _ => {}
            }
            a["op"] = json!(rop);
            a["path"] = json!(db);
            let v = call(&a).unwrap();
            let want = &st["result"];
            let got = if op == "set_last_post_date" { want.as_array().map(|w| if v["result"] == json!(true) { json!(w) } else { json!([false, w[1]]) }).unwrap() } else { v["result"].clone() };
            assert_eq!(&got, want, "step {n} {op} answer");
            let all = lists().lock().unwrap();
            let have: Vec<Value> = all[&db].list().iter().map(|e| json!({"rid": e.rid, "f": now_out(sub(&Value::Object(e.f.clone()), &b, "{BASE}"))})).collect();
            drop(all);
            assert_eq!(Value::Array(have), st["entries"], "step {n} {op} {}", st["args"]);
        }
        let mut names: Vec<String> = std::fs::read_dir(&base).unwrap().map(|d| d.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(json!(names), fx["folders"]);
        let rows: Vec<Value> = store::store(&db).load().unwrap().into_iter().map(|mut r| { r[2] = now_out(r[2].take()); sub(&r, &b, "{BASE}") }).collect();
        assert_eq!(Value::Array(rows), fx["db"]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
