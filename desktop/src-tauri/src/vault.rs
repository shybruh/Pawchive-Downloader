//! The Link Vault: cloud links (MEGA, Drive, Dropbox, GoFile…) and their passwords, kept per creator
//! and post in config/link_vault.db. A port of core/link_vault_manager.py: collecting links in,
//! the window's Creator > Posts > Links tree with its search and platform filter, editing
//! passwords, removing links / posts / creators, dead links, every password and link, the export
//! to text files, and the check of which links still work. The same database, rows and log lines;
//! the app keeps a copy it reads (passwords for the decompressor, links to download) and reloads
//! after every change made here.
//!
//! Checked against the Python vault by tests/fixtures/vault.json (tools/golden_vault.py).

use crate::downloader::albums::sanitize_filesystem_name;
use crate::downloader::service;
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

type Now = Box<dyn Fn() -> chrono::NaiveDateTime + Send>;
type NewId = Box<dyn Fn() -> String + Send>;

/// datetime.isoformat(): seconds, and microseconds when there are any
fn iso(t: chrono::NaiveDateTime) -> String {
    let us = t.and_utc().timestamp_subsec_micros();
    if us == 0 { t.format("%Y-%m-%dT%H:%M:%S").to_string() } else { format!("{}.{us:06}", t.format("%Y-%m-%dT%H:%M:%S")) }
}

fn uuid4() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// str(value) for what Python would print
fn pystr(v: &Value) -> String {
    crate::downloader::py::pystr(v)
}

/// `o[k1] or o[k2] or …` as text ("" when none)
fn first(o: &Value, keys: &[&str]) -> String {
    keys.iter().find_map(|k| o.get(*k).filter(|v| crate::downloader::filters::truthy(v)).map(pystr)).unwrap_or_default()
}

fn s(o: &Value, k: &str) -> String {
    o.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// core/text_utils.strip_html_tags: HTML as readable text
pub fn strip_html_tags(html: &str) -> String {
    if html.is_empty() {
        return String::new();
    }
    let t = re!(r"(?i)<br\s*/?>").replace_all(html, "\n");
    let t = re!(r"(?i)</p\s*>").replace_all(&t, "\n\n");
    let t = re!(r"(?i)</div\s*>").replace_all(&t, "\n");
    let t = re!(r"(?i)<li\s*>").replace_all(&t, "\n• ");
    let t = re!(r"<[^>]+>").replace_all(&t, "");
    let t = crate::downloader::py::html_unescape(&t);
    let t = re!(r"[\x{feff}\x{200b}-\x{200f}\x{2028}-\x{202f}\x{2060}-\x{206f}\x{00ad}\x{fffd}]").replace_all(&t, "");
    let t = t.replace("ï»¿", "");
    re!(r"\n{3,}").replace_all(&t, "\n\n").trim().to_string()
}

/// A link as the vault compares them (trailing slash dropped)
pub fn normalize_url(url: &str) -> String {
    let u = url.trim();
    if u.ends_with('/') && !u.ends_with("://") { u[..u.len() - 1].to_string() } else { u.to_string() }
}

#[derive(Default)]
struct Changes {
    creators: HashSet<String>,
    posts: HashSet<String>,
    links: HashSet<String>,
}

pub struct Vault {
    path: String,
    creators: Map<String, Value>,
    posts: Map<String, Value>,
    links: Vec<Value>,
    dirty: Changes,
    gone: Changes,
    seq: i64,
    seq_of: HashMap<(char, String), i64>,
    pub revision: u64,
    now: Now,
    new_id: NewId,
}

impl Vault {
    fn open(&self) -> rusqlite::Result<Connection> {
        if let Some(d) = std::path::Path::new(&self.path).parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let c = Connection::open(&self.path)?;
        c.busy_timeout(Duration::from_secs(30))?;
        c.query_row("PRAGMA journal_mode=WAL;", [], |_| Ok(()))?;
        c.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS creators (key TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS posts (post_id TEXT PRIMARY KEY, seq INTEGER, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS links (id TEXT PRIMARY KEY, seq INTEGER, data TEXT NOT NULL);",
        )?;
        Ok(c)
    }

    pub fn load(path: &str, now: Now, new_id: NewId) -> Result<Self, String> {
        let mut v = Vault {
            path: path.into(), creators: Map::new(), posts: Map::new(), links: Vec::new(), dirty: Changes::default(),
            gone: Changes::default(), seq: 0, seq_of: HashMap::new(), revision: 0, now, new_id,
        };
        if std::path::Path::new(path).exists() {
            let c = v.open().map_err(|e| e.to_string())?;
            let rows = |sql: &str| -> rusqlite::Result<Vec<(String, String)>> {
                let mut q = c.prepare(sql)?;
                let r = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.flatten().collect();
                Ok(r)
            };
            for (k, d) in rows("SELECT key, data FROM creators;").map_err(|e| e.to_string())? {
                v.creators.insert(k, serde_json::from_str(&d).unwrap_or(Value::Null));
            }
            for (k, d) in rows("SELECT post_id, data FROM posts ORDER BY seq;").map_err(|e| e.to_string())? {
                v.posts.insert(k, serde_json::from_str(&d).unwrap_or(Value::Null));
            }
            for (_, d) in rows("SELECT id, data FROM links ORDER BY seq;").map_err(|e| e.to_string())? {
                v.links.push(serde_json::from_str(&d).unwrap_or(Value::Null));
            }
        }
        for pid in v.posts.keys() {
            v.seq += 1;
            v.seq_of.insert(('p', pid.clone()), v.seq);
        }
        // (as the Python vault numbers them: one step per link, one more for its own number)
        let ids: Vec<String> = v.links.iter().map(|l| s(l, "id")).collect();
        for id in ids {
            v.seq += 1;
            if !id.is_empty() && !v.seq_of.contains_key(&('l', id.clone())) {
                v.seq += 1;
                v.seq_of.insert(('l', id), v.seq);
            }
        }
        Ok(v)
    }

    fn now(&self) -> String {
        iso((self.now)())
    }

    fn touch(&mut self, kind: char, key: &str) {
        if kind == 'p' && !self.seq_of.contains_key(&('p', key.to_string())) {
            self.seq += 1;
            self.seq_of.insert(('p', key.into()), self.seq);
        }
        if kind == 'l' && !self.seq_of.contains_key(&('l', key.to_string())) {
            self.seq += 1;
            self.seq_of.insert(('l', key.into()), self.seq);
        }
        let (d, g) = match kind {
            'c' => (&mut self.dirty.creators, &mut self.gone.creators),
            'p' => (&mut self.dirty.posts, &mut self.gone.posts),
            _ => (&mut self.dirty.links, &mut self.gone.links),
        };
        d.insert(key.into());
        g.remove(key);
    }

    fn forget(&mut self, kind: char, key: &str) {
        let (d, g) = match kind {
            'c' => (&mut self.dirty.creators, &mut self.gone.creators),
            'p' => (&mut self.dirty.posts, &mut self.gone.posts),
            _ => (&mut self.dirty.links, &mut self.gone.links),
        };
        d.remove(key);
        g.insert(key.into());
    }

    fn link_index(&self, id: &str) -> Option<usize> {
        self.links.iter().position(|l| s(l, "id") == id)
    }

    /// Writes what changed since the last save (one transaction)
    pub fn save(&mut self) -> bool {
        self.revision += 1;
        let creators: Vec<(String, String)> = self.dirty.creators.iter().filter_map(|k| self.creators.get(k).map(|v| (k.clone(), v.to_string()))).collect();
        let posts: Vec<(String, i64, String)> = self.dirty.posts.iter()
            .filter_map(|k| self.posts.get(k).map(|v| (k.clone(), *self.seq_of.get(&('p', k.clone())).unwrap_or(&0), v.to_string())))
            .collect();
        let links: Vec<(String, i64, String)> = self.links.iter()
            .filter(|l| self.dirty.links.contains(&s(l, "id")))
            .map(|l| {
                let id = s(l, "id");
                (id.clone(), *self.seq_of.get(&('l', id)).unwrap_or(&0), l.to_string())
            })
            .collect();
        if creators.is_empty() && posts.is_empty() && links.is_empty() && self.gone.creators.is_empty() && self.gone.posts.is_empty() && self.gone.links.is_empty() {
            return true;
        }
        let write = || -> rusqlite::Result<()> {
            let mut c = self.open()?;
            let tx = c.transaction()?;
            for (k, d) in &creators {
                tx.execute("INSERT INTO creators (key, data) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET data = excluded.data;", params![k, d])?;
            }
            for (k, seq, d) in &posts {
                tx.execute("INSERT INTO posts (post_id, seq, data) VALUES (?1, ?2, ?3) ON CONFLICT(post_id) DO UPDATE SET data = excluded.data;", params![k, seq, d])?;
            }
            for (k, seq, d) in &links {
                tx.execute("INSERT INTO links (id, seq, data) VALUES (?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET data = excluded.data;", params![k, seq, d])?;
            }
            for k in &self.gone.creators {
                tx.execute("DELETE FROM creators WHERE key = ?1;", params![k])?;
            }
            for k in &self.gone.posts {
                tx.execute("DELETE FROM posts WHERE post_id = ?1;", params![k])?;
            }
            for k in &self.gone.links {
                tx.execute("DELETE FROM links WHERE id = ?1;", params![k])?;
            }
            tx.commit()
        };
        if let Err(e) = write() {
            log("error", &format!("Failed to persist link vault: {e}"));
            return false;
        }
        self.dirty = Changes::default();
        self.gone = Changes::default();
        true
    }

    fn recount(&mut self, only: &HashSet<String>) {
        let keys: Vec<String> = only.iter().filter(|k| self.creators.contains_key(*k)).cloned().collect();
        for k in keys {
            let p = self.posts.values().filter(|p| s(p, "creator_key") == k).count();
            let l = self.links.iter().filter(|l| s(l, "creator_key") == k).count();
            let c = self.creators.get_mut(&k).unwrap();
            if c.get("post_count") != Some(&json!(p)) || c.get("link_count") != Some(&json!(l)) {
                c["post_count"] = json!(p);
                c["link_count"] = json!(l);
                self.touch('c', &k);
            }
        }
    }

    /// Collected posts (with their links and passwords) into the vault: how many links are new
    pub fn harvest(&mut self, creator_name: &str, service: &str, user_id: &str, posts: &[Value]) -> usize {
        if posts.is_empty() {
            return 0;
        }
        let name = { let n = creator_name.trim(); if n.is_empty() { "Unknown Artist" } else { n } }.to_string();
        let service = { let n = service.trim(); if n.is_empty() { "general" } else { n } }.to_string();
        let uid = { let n = user_id.trim(); if n.is_empty() { "unknown" } else { n } }.to_string();
        let key = format!("{service}:{uid}").to_lowercase();
        let now = self.now();
        match self.creators.get_mut(&key) {
            Some(c) => {
                c["creator_name"] = json!(name);
                c["updated_at"] = json!(now);
            }
            None => {
                self.creators.insert(key.clone(), json!({
                    "creator_key": key, "creator_name": name, "service": service, "user_id": uid,
                    "post_count": 0, "link_count": 0, "created_at": now, "updated_at": now,
                }));
            }
        }
        self.touch('c', &key);
        let mut new_links = 0;
        for p in posts {
            let mut pid = first(p, &["post_id", "id"]);
            if pid.is_empty() {
                pid = (self.new_id)().chars().take(8).collect();
            }
            let title = { let t = first(p, &["title"]); if t.is_empty() { "Untitled Post".to_string() } else { t } }.trim().to_string();
            let published = first(p, &["published", "date"]);
            let body = first(p, &["full_text", "content", "description"]);
            let post_url = first(p, &["post_url", "url"]);
            let post_pws: Vec<Value> = p.get("passwords").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut unique: Vec<String> = post_pws.iter().map(pystr).collect::<HashSet<_>>().into_iter().collect();
            unique.sort();      // (Python keeps them in a set's order: any order)
            self.posts.insert(pid.clone(), json!({
                "post_id": pid, "creator_key": key, "creator_name": name, "title": title, "published": published,
                "full_text": strip_html_tags(&body), "post_url": post_url, "passwords": unique,
            }));
            self.touch('p', &pid);
            for item in p.get("links").and_then(Value::as_array).cloned().unwrap_or_default() {
                let (raw, platform, pws, source) = match &item {
                    Value::String(u) => (u.clone(), "other".to_string(), post_pws.clone(), "post".to_string()),
                    Value::Object(o) => {
                        let pws = o.get("passwords").filter(|v| crate::downloader::filters::truthy(v)).and_then(Value::as_array).cloned().unwrap_or_else(|| post_pws.clone());
                        (s(&item, "url"), o.get("platform").map(pystr).unwrap_or_else(|| "other".into()), pws,
                         o.get("password_source").map(pystr).unwrap_or_else(|| "post".into()))
                    }
                    _ => continue,
                };
                let norm = normalize_url(&raw);
                if norm.is_empty() {
                    continue;
                }
                let pws: Vec<String> = pws.iter().map(pystr).collect();
                let existing = self.links.iter().position(|l| s(l, "creator_key") == key && normalize_url(&s(l, "url")) == norm);
                if let Some(i) = existing {
                    let mut cur: HashSet<String> = self.links[i].get("passwords").and_then(Value::as_array).map(|a| a.iter().map(pystr).collect()).unwrap_or_default();
                    if !pws.iter().all(|p| cur.contains(p)) {
                        cur.extend(pws);
                        let mut all: Vec<String> = cur.into_iter().collect();
                        all.sort();
                        self.links[i]["passwords"] = json!(all);
                        let id = s(&self.links[i], "id");
                        self.touch('l', &id);
                    }
                    continue;
                }
                let mut sorted: Vec<String> = pws.into_iter().collect::<HashSet<_>>().into_iter().collect();
                sorted.sort();
                let id = (self.new_id)();
                self.links.push(json!({
                    "id": id, "url": norm, "platform": platform, "post_id": pid, "creator_key": key, "creator_name": name,
                    "post_title": title, "passwords": sorted, "password_source": source, "health": "unknown",
                    "last_checked": null, "added_at": now,
                }));
                self.touch('l', &id);
                new_links += 1;
            }
        }
        self.recount(&HashSet::from([key]));
        self.save();
        if new_links > 0 {
            log("success", &format!("Link Vault: Added {new_links} new link(s) for '{name}'."));
        }
        new_links
    }

    /// The window's Creator > Posts > Links tree, newest creators first, with the search (creator,
    /// post, link or password) and the platform filter
    pub fn tree(&self, search: &str, platform: &str) -> Vec<Value> {
        let q = search.trim().to_lowercase();
        let plat = platform.trim().to_lowercase();
        let mut by_creator: HashMap<String, Vec<&Value>> = HashMap::new();
        for p in self.posts.values() {
            by_creator.entry(s(p, "creator_key")).or_default().push(p);
        }
        let mut by_post: HashMap<(String, String), Vec<&Value>> = HashMap::new();
        for l in &self.links {
            by_post.entry((s(l, "creator_key"), s(l, "post_id"))).or_default().push(l);
        }
        let mut creators: Vec<&Value> = self.creators.values().collect();
        creators.sort_by_key(|c| std::cmp::Reverse(s(c, "updated_at")));
        let mut tree = Vec::new();
        for c in creators {
            let key = s(c, "creator_key");
            let name = c.get("creator_name").and_then(Value::as_str).unwrap_or("Unknown").to_string();
            let svc = s(c, "service");
            let mut posts = by_creator.get(&key).cloned().unwrap_or_default();
            posts.sort_by_key(|p| std::cmp::Reverse(s(p, "published")));
            let mut list = Vec::new();
            let mut total = 0;
            for p in posts {
                let pid = s(p, "post_id");
                let title = s(p, "title");
                let text = s(p, "full_text");
                let mut links: Vec<&Value> = by_post.get(&(key.clone(), pid.clone())).cloned().unwrap_or_default();
                if !plat.is_empty() && plat != "all" {
                    links.retain(|l| s(l, "platform").to_lowercase() == plat);
                }
                if !q.is_empty() {
                    let creator_hit = name.to_lowercase().contains(&q) || svc.to_lowercase().contains(&q);
                    let post_hit = title.to_lowercase().contains(&q) || text.to_lowercase().contains(&q);
                    links.retain(|l| {
                        creator_hit || post_hit || s(l, "url").to_lowercase().contains(&q)
                            || l.get("passwords").and_then(Value::as_array).is_some_and(|a| a.iter().any(|pw| pystr(pw).to_lowercase().contains(&q)))
                    });
                }
                if links.is_empty() && !q.is_empty() && !(name.to_lowercase().contains(&q) || title.to_lowercase().contains(&q)) {
                    continue;
                }
                total += links.len();
                list.push(json!({
                    "post_id": pid, "title": title, "published": s(p, "published"), "full_text": strip_html_tags(&text),
                    "post_url": s(p, "post_url"), "passwords": p.get("passwords").cloned().unwrap_or(json!([])),
                    "links_count": links.len(), "links": links,
                }));
            }
            if !list.is_empty() || (q.is_empty() && plat.is_empty()) {
                tree.push(json!({
                    "creator_key": key, "creator_name": name, "service": svc, "user_id": s(c, "user_id"),
                    "post_count": list.len(), "link_count": total, "updated_at": s(c, "updated_at"), "posts": list,
                }));
            }
        }
        tree
    }

    pub fn delete_link(&mut self, id: &str) -> bool {
        let Some(i) = self.link_index(id) else { return false };
        let l = self.links.remove(i);
        self.links.retain(|x| s(x, "id") != id);
        self.forget('l', id);
        self.recount(&HashSet::from([s(&l, "creator_key")]));
        self.save();
        log("info", &format!("Link {id} deleted from Link Vault."));
        true
    }

    pub fn set_passwords(&mut self, id: &str, passwords: &[Value]) -> bool {
        let mut clean: Vec<String> = Vec::new();
        for p in passwords {
            let v = pystr(p).trim().to_string();
            if !v.is_empty() && !clean.contains(&v) {
                clean.push(v);
            }
        }
        let Some(i) = self.link_index(id) else { return false };
        let n = clean.len();
        self.links[i]["passwords"] = json!(clean);
        self.touch('l', id);
        self.save();
        log("info", &format!("Updated passwords for link {id} ({n} saved)."));
        true
    }

    pub fn delete_post(&mut self, pid: &str) -> bool {
        let mut affected = HashSet::new();
        let removed = match self.posts.shift_remove(pid) {
            Some(p) => {
                affected.insert(s(&p, "creator_key"));
                self.forget('p', pid);
                true
            }
            None => false,
        };
        let gone: Vec<Value> = self.links.iter().filter(|l| s(l, "post_id") == pid).cloned().collect();
        if !gone.is_empty() {
            self.links.retain(|l| s(l, "post_id") != pid);
            for l in &gone {
                affected.insert(s(l, "creator_key"));
                self.forget('l', &s(l, "id"));
            }
        }
        if removed || !gone.is_empty() {
            self.recount(&affected);
            self.save();
            log("info", &format!("Post {pid} and child links deleted from Link Vault."));
            return true;
        }
        false
    }

    pub fn delete_creator(&mut self, key: &str) -> bool {
        let key = key.to_lowercase();
        if self.creators.shift_remove(&key).is_none() {
            return false;
        }
        self.forget('c', &key);
        let posts: Vec<String> = self.posts.iter().filter(|(_, p)| s(p, "creator_key") == key).map(|(k, _)| k.clone()).collect();
        for pid in posts {
            self.posts.shift_remove(&pid);
            self.forget('p', &pid);
        }
        let gone: Vec<String> = self.links.iter().filter(|l| s(l, "creator_key") == key).map(|l| s(l, "id")).collect();
        self.links.retain(|l| s(l, "creator_key") != key);
        for id in gone {
            self.forget('l', &id);
        }
        self.save();
        log("info", &format!("Creator {key} purged from Link Vault."));
        true
    }

    pub fn clean_dead(&mut self) -> usize {
        let dead: Vec<Value> = self.links.iter().filter(|l| s(l, "health") == "dead").cloned().collect();
        if dead.is_empty() {
            return 0;
        }
        self.links.retain(|l| s(l, "health") != "dead");
        for l in &dead {
            self.forget('l', &s(l, "id"));
        }
        self.recount(&dead.iter().map(|l| s(l, "creator_key")).collect());
        self.save();
        log("success", &format!("Cleaned {} dead links from Link Vault.", dead.len()));
        dead.len()
    }

    /// Links checked: their health ("alive", "dead", "unknown") and when
    pub fn set_health(&mut self, updates: &[(String, String)], save: bool) {
        let now = self.now();
        for (id, h) in updates {
            if let Some(i) = self.link_index(id) {
                self.links[i]["health"] = json!(h);
                self.links[i]["last_checked"] = json!(now);
                self.touch('l', id);
            }
        }
        if save {
            self.save();
        }
    }

    pub fn all_passwords(&self) -> Vec<String> {
        let mut set = HashSet::new();
        for list in self.posts.values().chain(self.links.iter()).filter_map(|x| x.get("passwords").and_then(Value::as_array)) {
            for pw in list.iter().filter_map(Value::as_str).filter(|p| !p.is_empty()) {
                set.insert(pw.trim().to_string());
            }
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        v
    }

    pub fn urls(&self, creator: &str) -> Vec<String> {
        let ck = creator.to_lowercase().trim().to_string();
        let mut out: Vec<String> = Vec::new();
        for l in &self.links {
            if !ck.is_empty() && s(l, "creator_key").to_lowercase() != ck {
                continue;
            }
            let u = s(l, "url");
            if !u.is_empty() && !out.contains(&u) {
                out.push(u);
            }
        }
        out
    }

    /// One text file per creator with links: posts newest first, each link with its passwords
    pub fn export(&self, dir: &str) -> usize {
        if dir.is_empty() || std::fs::create_dir_all(dir).is_err() {
            return 0;
        }
        let mut by_creator: HashMap<String, Vec<&Value>> = HashMap::new();
        for p in self.posts.values() {
            by_creator.entry(s(p, "creator_key")).or_default().push(p);
        }
        let mut by_post: HashMap<(String, String), Vec<&Value>> = HashMap::new();
        for l in &self.links {
            by_post.entry((s(l, "creator_key"), s(l, "post_id"))).or_default().push(l);
        }
        let stamp = (self.now)().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut n = 0;
        for (key, c) in &self.creators {
            let name = c.get("creator_name").and_then(Value::as_str).unwrap_or("Unknown").to_string();
            let svc = s(c, "service");
            let mut posts = by_creator.get(key).cloned().unwrap_or_default();
            posts.sort_by_key(|p| std::cmp::Reverse(s(p, "published")));
            let total: usize = posts.iter().map(|p| by_post.get(&(key.clone(), s(p, "post_id"))).map_or(0, Vec::len)).sum();
            if total == 0 {
                continue;
            }
            let title = sanitize_filesystem_name(&if svc.is_empty() { name.clone() } else { format!("{name} [{svc}]") }, "item", 180);
            let mut lines = vec![format!("Creator: {name} [{svc}]"), format!("Total Links: {total}"), format!("Exported: {stamp}"), "=".repeat(80), String::new()];
            for p in posts {
                let pid = s(p, "post_id");
                let Some(links) = by_post.get(&(key.clone(), pid.clone())).filter(|l| !l.is_empty()) else { continue };
                let t = first(p, &["title"]);
                lines.push(format!("📌 {}", if t.is_empty() { format!("Post {pid}") } else { t }));
                let url = s(p, "url");          // (posts keep "post_url": as before, no URL line)
                if !url.is_empty() {
                    lines.push(format!("   URL: {url}"));
                }
                let date = s(p, "published");
                if !date.is_empty() {
                    lines.push(format!("   Date: {date}"));
                }
                lines.push("   Links:".into());
                for l in links {
                    let pws: Vec<String> = l.get("passwords").and_then(Value::as_array).map(|a| a.iter().map(pystr).collect()).unwrap_or_default();
                    let pw = if pws.is_empty() { String::new() } else { format!(" (Passwords: {})", pws.join(", ")) };
                    lines.push(format!("     - {}{pw}", s(l, "url")));
                }
                lines.push(String::new());
            }
            let path = std::path::Path::new(dir).join(format!("{title}.txt"));
            let tmp = path.with_extension("txt.tmp");
            if std::fs::write(&tmp, lines.join("\n")).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
                n += 1;
            }
        }
        n
    }

    pub fn links_of(&self, creator: Option<&str>) -> Vec<(String, String)> {
        let ck = creator.map(str::to_lowercase);
        self.links.iter().filter(|l| ck.as_ref().is_none_or(|c| s(l, "creator_key").to_lowercase() == *c)).map(|l| (s(l, "id"), s(l, "url"))).collect()
    }

    pub fn data(&self) -> Value {
        json!({"version": 1, "creators": self.creators, "posts": self.posts, "links": self.links})
    }
}

fn log(level: &str, msg: &str) {
    service::log_app(level, msg, "vault", "");
}

// ── Which links still work ────────────────────────────────────────────────────────────────────

fn client() -> &'static reqwest::blocking::Client {
    static C: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::blocking::Client::builder().timeout(Duration::from_secs(6)).build().unwrap_or_default())
}

fn probe_mega(url: &str) -> String {
    let (handle, folder) = match re!(r"mega(?:\.co)?\.nz/(?:file/|#!)([A-Za-z0-9_-]{8})").captures(url) {
        Some(c) => (c[1].to_string(), false),
        None => match re!(r"mega(?:\.co)?\.nz/(?:folder/|#F!)([A-Za-z0-9_-]{8})").captures(url) {
            Some(c) => (c[1].to_string(), true),
            None => return "unknown".into(),
        },
    };
    let r = if folder {
        client().post(format!("https://g.api.mega.co.nz/cs?id=1&n={handle}")).timeout(Duration::from_secs(8)).json(&json!([{"a": "f", "c": 1, "ca": 1, "r": 1}])).send()
    } else {
        client().post("https://g.api.mega.co.nz/cs?id=1").timeout(Duration::from_secs(8)).json(&json!([{"a": "g", "p": handle}])).send()
    };
    let Ok(r) = r else { return "unknown".into() };
    if r.status().as_u16() != 200 {
        return "unknown".into();
    }
    let Ok(v) = r.json::<Value>() else { return "unknown".into() };
    let first = match &v {
        Value::Array(a) if !a.is_empty() => a[0].clone(),
        other => other.clone(),
    };
    if first.is_object() {
        "alive".into()
    } else if matches!(first.as_i64(), Some(-9) | Some(-16)) {
        "dead".into()
    } else {
        "unknown".into()
    }
}

/// Whether one link still works: "alive", "dead" or "unknown"
pub fn probe(url: &str) -> String {
    if url.is_empty() {
        return "dead".into();
    }
    let c = client();
    if url.contains("pixeldrain.com") {
        if let Some(m) = re!(r"pixeldrain\.com/(?:u/|l/|api/file/)([a-zA-Z0-9_\-]+)").captures(url) {
            let Ok(r) = c.get(format!("https://pixeldrain.com/api/file/{}/info", &m[1])).send() else { return "unknown".into() };
            let code = r.status().as_u16();
            let Ok(v) = r.json::<Value>() else { return "unknown".into() };
            if code == 200 && v.get("success") == Some(&json!(true)) {
                return "alive".into();
            } else if code == 404 {
                return "dead".into();
            }
        }
    }
    if url.contains("gofile.io") {
        if let Some(m) = re!(r"gofile\.io/(?:d/|#)([a-zA-Z0-9_\-]+)").captures(url) {
            let Ok(r) = c.get(format!("https://api.gofile.io/contents/{}", &m[1])).send() else { return "unknown".into() };
            let code = r.status().as_u16();
            let Ok(v) = r.json::<Value>() else { return "unknown".into() };
            if code == 200 && v.get("status") == Some(&json!("ok")) {
                return "alive".into();
            } else if matches!(code, 404 | 400) {
                return "dead".into();
            }
        }
    }
    if url.contains("mega.nz") || url.contains("mega.co.nz") {
        return probe_mega(url);
    }
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";
    let Ok(r) = c.head(url).header("User-Agent", ua).send() else { return "unknown".into() };
    match r.status().as_u16() {
        200 | 206 | 301 | 302 | 307 | 308 => "alive".into(),
        404 | 410 => "dead".into(),
        405 => match c.get(url).header("User-Agent", ua).header("Range", "bytes=0-0").send().map(|r| r.status().as_u16()) {
            Ok(200 | 206) => "alive".into(),
            Ok(404 | 410) => "dead".into(),
            _ => "unknown".into(),
        },
        _ => "unknown".into(),
    }
}

// ── The vaults, by file ───────────────────────────────────────────────────────────────────────

fn vaults() -> &'static Mutex<HashMap<String, Arc<Mutex<Vault>>>> {
    static V: OnceLock<Mutex<HashMap<String, Arc<Mutex<Vault>>>>> = OnceLock::new();
    V.get_or_init(Default::default)
}

fn vault(path: &str) -> Result<Arc<Mutex<Vault>>, String> {
    let mut all = vaults().lock().unwrap();
    if let Some(v) = all.get(path) {
        return Ok(v.clone());
    }
    let v = Arc::new(Mutex::new(Vault::load(path, Box::new(|| chrono::Local::now().naive_local()), Box::new(uuid4))?));
    all.insert(path.into(), v.clone());
    Ok(v)
}

static PROBING: AtomicBool = AtomicBool::new(false);

/// Checks the vault's links (or a creator's), 8 at a time, saving every 50; progress notes
/// {"done", "total", "url"}
fn probe_all(id: u64, path: &str, creator: Option<&str>) -> Result<Value, String> {
    if PROBING.swap(true, Ordering::SeqCst) {
        log("warning", "Health probing is already in progress.");
        return Ok(json!({"done": 0, "total": 0, "busy": true}));
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            PROBING.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done;
    let v = vault(path)?;
    let links = v.lock().unwrap().links_of(creator);
    let total = links.len();
    log("info", &format!("Link Vault: Starting health probe across {total} links (creator={})...", creator.unwrap_or("all")));
    let cancel = service::cancel_flag(id);
    let next = Mutex::new(0usize);
    let done = Mutex::new(0usize);
    std::thread::scope(|sc| {
        for _ in 0..8.min(total.max(1)) {
            sc.spawn(|| loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let i = {
                    let mut n = next.lock().unwrap();
                    *n += 1;
                    *n - 1
                };
                if i >= total {
                    break;
                }
                let (lid, url) = &links[i];
                let h = probe(url);
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let n = {
                    let mut d = done.lock().unwrap();
                    *d += 1;
                    *d
                };
                let mut g = v.lock().unwrap();
                g.set_health(&[(lid.clone(), h)], n % 50 == 0);
                drop(g);
                service::progress_value(id, json!({"done": n, "total": total, "url": url}));
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        log("warning", "Health probing cancelled by user.");
    }
    let mut g = v.lock().unwrap();
    g.save();
    let n = *done.lock().unwrap();
    log("success", &format!("Link Vault: Health probing complete ({n}/{total} checked)."));
    Ok(json!({"done": n, "total": total, "revision": g.revision}))
}

/// Call "vault": {"op", "path", …} → the op's answer, with the vault's revision (the app reloads
/// its copy when it changed)
pub fn call(id: u64, a: &Value) -> Result<Value, String> {
    let path = a["path"].as_str().ok_or("no path")?;
    let op = a["op"].as_str().unwrap_or("");
    if op == "probe" {
        return probe_all(id, path, a["creator"].as_str().filter(|c| !c.is_empty()));
    }
    let v = vault(path)?;
    let mut g = v.lock().unwrap();
    let st = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let result = match op {
        "reload" => {
            // (the app changed the file itself: read it again)
            let fresh = Vault::load(path, Box::new(|| chrono::Local::now().naive_local()), Box::new(uuid4))?;
            let rev = g.revision + 1;
            *g = fresh;
            g.revision = rev;
            Value::Null
        }
        "harvest" => json!(g.harvest(&st("creator_name"), &st("service"), &st("user_id"), a["posts"].as_array().map(Vec::as_slice).unwrap_or(&[]))),
        "tree" => json!(g.tree(&st("search"), &st("platform"))),
        "delete_link" => json!(g.delete_link(&st("id"))),
        "passwords_set" => json!(g.set_passwords(&st("id"), a["passwords"].as_array().map(Vec::as_slice).unwrap_or(&[]))),
        "delete_post" => json!(g.delete_post(&st("id"))),
        "delete_creator" => json!(g.delete_creator(&st("key"))),
        "clean_dead" => json!(g.clean_dead()),
        "health" => {
            let u: Vec<(String, String)> = a["updates"].as_array().into_iter().flatten()
                .filter_map(|x| Some((x.get(0)?.as_str()?.to_string(), x.get(1)?.as_str()?.to_string()))).collect();
            g.set_health(&u, true);
            Value::Null
        }
        "all_passwords" => json!(g.all_passwords()),
        "urls" => json!(g.urls(&st("creator"))),
        "export" => json!(g.export(&st("dir"))),
        "data" => g.data(),
        other => return Err(format!("unknown vault op {other}")),
    };
    Ok(json!({"result": result, "revision": g.revision}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static CLOCK: Mutex<String> = Mutex::new(String::new());
    static IDS: AtomicUsize = AtomicUsize::new(0);

    /// Post passwords in a set's order in Python: compared sorted
    fn tidy(mut v: Value) -> Value {
        fn walk(v: &mut Value) {
            match v {
                Value::Object(m) => {
                    if m.contains_key("post_id") && !m.contains_key("url") {
                        if let Some(Value::Array(a)) = m.get_mut("passwords") {
                            a.sort_by_key(|x| x.to_string());
                        }
                    }
                    for x in m.values_mut() {
                        walk(x);
                    }
                }
                Value::Array(a) => a.iter_mut().for_each(walk),
                _ => {}
            }
        }
        walk(&mut v);
        v
    }

    #[test]
    fn matches_python_vault() {
        let fx: Value = serde_json::from_str(include_str!("../tests/fixtures/vault.json")).unwrap();
        let dir = std::env::temp_dir().join(format!("pawchive_vault_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("link_vault.db").to_string_lossy().to_string();
        let now: Now = Box::new(|| chrono::NaiveDateTime::parse_from_str(&CLOCK.lock().unwrap(), "%Y-%m-%dT%H:%M:%S").unwrap());
        let ids: NewId = Box::new(|| format!("id-{}-0000-0000-0000-000000000000", IDS.fetch_add(1, Ordering::SeqCst) + 1));
        let mut v = Vault::load(&path, now, ids).unwrap();
        for (i, st) in fx["steps"].as_array().unwrap().iter().enumerate() {
            *CLOCK.lock().unwrap() = st["time"].as_str().unwrap().to_string();
            let a = &st["args"];
            let t = |k: &str| a[k].as_str().unwrap_or("").to_string();
            let export = dir.join("export");
            let got = match st["op"].as_str().unwrap() {
                "harvest" => json!(v.harvest(&t("creator_name"), &t("service"), &t("user_id"), a["posts"].as_array().unwrap())),
                "tree" => json!(v.tree(&t("search"), &t("platform"))),
                "passwords_set" => json!(v.set_passwords(&t("id"), a["passwords"].as_array().unwrap())),
                "health" => {
                    let u: Vec<(String, String)> = a["updates"].as_array().unwrap().iter().map(|x| (x[0].as_str().unwrap().into(), x[1].as_str().unwrap().into())).collect();
                    v.set_health(&u, true);
                    Value::Null
                }
                "all_passwords" => json!(v.all_passwords()),
                "urls" => json!(v.urls(&t("creator"))),
                "export" => {
                    let n = v.export(&export.to_string_lossy());
                    let mut files = Map::new();
                    let mut names: Vec<_> = std::fs::read_dir(&export).unwrap().flatten().map(|e| e.path()).collect();
                    names.sort();
                    for f in names {
                        files.insert(f.file_name().unwrap().to_string_lossy().to_string(), json!(std::fs::read_to_string(&f).unwrap()));
                    }
                    json!({"count": n, "files": files})
                }
                "clean_dead" => json!(v.clean_dead()),
                "delete_link" => json!(v.delete_link(&t("id"))),
                "delete_post" => json!(v.delete_post(&t("id"))),
                "delete_creator" => json!(v.delete_creator(&t("key"))),
                op => panic!("{op}"),
            };
            assert_eq!(tidy(got), tidy(st["result"].clone()), "step {i} {}", st["op"]);
            assert_eq!(tidy(v.data()), tidy(st["data"].clone()), "vault after step {i} {}", st["op"]);
        }
        // The database rows, as the Python vault left them
        let c = Connection::open(&path).unwrap();
        let mut q = c.prepare("SELECT key, data FROM creators ORDER BY key;").unwrap();
        let creators: Vec<Value> = q.query_map([], |r| Ok(json!([r.get::<_, String>(0)?, serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap()]))).unwrap().flatten().collect();
        assert_eq!(Value::Array(creators), fx["rows"]["creators"]);
        for (t, k) in [("posts", "post_id"), ("links", "id")] {
            let mut q = c.prepare(&format!("SELECT {k}, seq, data FROM {t} ORDER BY {k};")).unwrap();
            let rows: Vec<Value> = q.query_map([], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, i64>(1)?, serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap()]))).unwrap().flatten().collect();
            assert_eq!(tidy(Value::Array(rows)), tidy(fx["rows"][t].clone()), "{t} rows");
        }
        drop(q);
        drop(c);
        // …and read back the same
        let again = Vault::load(&path, Box::new(|| chrono::Local::now().naive_local()), Box::new(uuid4)).unwrap();
        assert_eq!(tidy(again.data()), tidy(fx["steps"].as_array().unwrap().last().unwrap()["data"].clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
