//! The window's watchlist: the artists filtered by the search and the category (all, updates,
//! favorites, on disk, a site), artists with new posts first and then by name, the counts, each
//! row's fields and whether the artist's folder exists. A port of bridge/watchlist_model.py,
//! checked against it by tools/golden_watchlist_view.py.
//!
//! The app sends the entries (`apply`); the host serves the result to the window
//! (host/watchlist.rs) as "appBridge.watchlistModel".

use crate::downloader::naming::creator_folder_names;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// One artist, as the app sends it
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct Row {
    pub key: String,
    pub url: String,
    pub creator_name: String,
    pub service: String,
    pub domain: String,
    pub user_id: String,
    pub last_post_date: String,
    pub last_post_id: String,
    pub added_at: String,
    pub auto_check: bool,
    pub new_post_count: i64,
    pub download_dir: String,
    pub download_dirs: Vec<String>,
    pub ignored_count: i64,
    /// The new posts a check found (only the fields the list shows)
    pub posts: Vec<Value>,
    pub is_favorite: bool,
}

pub const ROLES: &[&str] = &[
    "url", "creatorName", "service", "domain", "userId", "lastPostDate", "lastPostId", "addedAt", "autoCheck", "newPostCount",
    "downloadDir", "ignoredCount", "cachedNewPosts", "downloadDirs", "isFavorite", "provider", "isOnDisk", "display",
];

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

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        other => other.to_string(),
    }
}

/// How many files a found post has (its main file and attachments, each path once)
fn file_count(p: &Value) -> i64 {
    let key = |f: &Value| ["path", "storageKey"].iter().find_map(|k| f.get(*k).filter(|v| truthy(v))).map(text).unwrap_or_default().trim().to_lowercase();
    let atts: Vec<&Value> = p
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| a.is_object() && (a.get("path").is_some_and(truthy) || a.get("storageKey").is_some_and(truthy)))
        .collect();
    let f = p.get("file").filter(|f| truthy(f));
    let f_path = f.filter(|f| f.is_object()).map(key).unwrap_or_default();
    let att_paths: HashSet<String> = atts.iter().map(|a| key(a)).collect();
    let mut unique = att_paths.len() as i64;
    if !f_path.is_empty() && !att_paths.contains(&f_path) {
        unique += 1;
    }
    if unique > 0 {
        unique
    } else if f.is_some() {
        1
    } else {
        atts.len() as i64
    }
}

fn posts_for_window(r: &Row) -> Vec<Value> {
    r.posts
        .iter()
        .map(|p| {
            let pick = |ks: &[&str]| ks.iter().find_map(|k| p.get(*k).filter(|v| truthy(v)).cloned());
            let title = pick(&["title", "captionText", "caption"]).map(|v| text(&v)).unwrap_or_else(|| "Untitled".into());
            let published: String = pick(&["published", "added"]).map(|v| text(&v)).unwrap_or_default().chars().take(10).collect();
            json!({
                "id": p.get("id").map(text).unwrap_or_default(),
                "title": title.trim(),
                "published": published,
                "fileCount": file_count(p),
            })
        })
        .collect()
}

fn download_dirs(r: &Row) -> Vec<String> {
    let mut d = r.download_dirs.clone();
    if !r.download_dir.is_empty() && !d.contains(&r.download_dir) {
        d.insert(0, r.download_dir.clone());
    }
    d
}

fn provider(r: &Row) -> &'static str {
    let dom = r.domain.to_lowercase();
    if dom.contains("cum.st") || dom.contains("cumst") {
        "cumst"
    } else if matches!(crate::downloader::links::archive_site(&dom), Some("kemono" | "coomer")) {
        "unsupported"
    } else {
        "pawchive"
    }
}

/// Folders asked about during one look at the list
#[derive(Default)]
struct Disk {
    dirs: HashMap<String, bool>,
    /// The download folder's subfolders (lower case), when it was read once for many artists
    subdirs: Option<HashSet<String>>,
}

impl Disk {
    fn is_dir(&mut self, p: &str) -> bool {
        *self.dirs.entry(p.to_string()).or_insert_with(|| Path::new(p).is_dir())
    }
}

#[derive(Debug, Default)]
pub struct View {
    rows: Vec<Row>,
    at: HashMap<String, usize>,
    pub search: String,
    pub filter: String,
    /// The download folder, where an artist's folder is looked for
    pub base_dir: String,
    /// "Site in folder name": "Artist [service]"
    pub include_service: bool,
    pub dirty: bool,
    on_disk_total: Option<usize>,
}

pub struct Built {
    /// Indexes into the rows, in the order shown
    pub shown: Vec<usize>,
    pub on_disk: HashMap<usize, bool>,
    pub total: usize,
    pub updated: usize,
    pub new_posts: i64,
}

impl View {
    pub fn new() -> Self {
        Self { filter: "all".into(), include_service: true, ..Default::default() }
    }

    /// The app's changes: `order` (when given) is every artist's key in watchlist order; `rows`
    /// are new or changed artists
    pub fn apply(&mut self, order: Option<Vec<String>>, rows: Vec<Row>) {
        for r in rows {
            match self.at.get(&r.key) {
                Some(&i) => self.rows[i] = r,
                None => {
                    self.at.insert(r.key.clone(), self.rows.len());
                    self.rows.push(r);
                }
            }
        }
        if let Some(order) = order {
            let mut by: HashMap<String, Row> = self.rows.drain(..).map(|r| (r.key.clone(), r)).collect();
            self.rows = order.into_iter().filter_map(|k| by.remove(&k)).collect();
            self.at = self.rows.iter().enumerate().map(|(i, r)| (r.key.clone(), i)).collect();
            self.on_disk_total = None;
        }
        self.dirty = true;
    }

    fn base_subdirs(&self) -> HashSet<String> {
        std::fs::read_dir(&self.base_dir)
            .map(|rd| rd.flatten().filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).map(|e| e.file_name().to_string_lossy().to_lowercase()).collect())
            .unwrap_or_default()
    }

    fn on_disk(&self, r: &Row, disk: &mut Disk) -> bool {
        // 1. one of the artist's own folders exists
        let mut dirs: Vec<&String> = Vec::new();
        if !r.download_dir.is_empty() {
            dirs.push(&r.download_dir);
        }
        for d in &r.download_dirs {
            if !d.is_empty() && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        if dirs.iter().any(|d| disk.is_dir(d)) {
            return true;
        }
        // 2. a folder named after the artist in the download folder
        if self.base_dir.is_empty() {
            return false;
        }
        let name = if r.creator_name.is_empty() { &r.user_id } else { &r.creator_name };
        let names = creator_folder_names(name, &r.service, self.include_service);
        match &disk.subdirs {
            Some(sub) => names.iter().any(|n| sub.contains(&n.to_lowercase())),
            None => names.iter().any(|n| {
                let p = crate::downloader::py::join(&[&self.base_dir, n]);
                disk.is_dir(&p)
            }),
        }
    }

    /// The list as shown: filtered, artists with new posts first, then by name
    pub fn build(&self) -> Built {
        let st = self.search.trim().to_lowercase();
        let sf = self.filter.trim().to_lowercase();
        let sf = if sf.is_empty() { "all".to_string() } else { sf };
        let filter_disk = matches!(sf.as_str(), "synced" | "on_disk" | "ondisk");
        let mut disk = Disk::default();
        if filter_disk && !self.base_dir.is_empty() && Path::new(&self.base_dir).is_dir() {
            disk.subdirs = Some(self.base_subdirs());
        }
        let (mut updated, mut new_posts) = (0usize, 0i64);
        let mut on_disk = HashMap::new();
        let mut keyed: Vec<(u8, String, usize)> = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            let n = r.new_post_count;
            if n > 0 {
                updated += 1;
                new_posts += n;
            }
            let mut is_disk = false;
            if filter_disk {
                is_disk = self.on_disk(r, &mut disk);
                on_disk.insert(i, is_disk);
            }
            if !st.is_empty() && !r.creator_name.to_lowercase().contains(&st) && !r.user_id.to_lowercase().contains(&st) {
                continue;
            }
            let keep = match sf.as_str() {
                "updates" => n > 0,
                "favorites" | "favorite" => r.is_favorite,
                _ if filter_disk => is_disk,
                "all" => true,
                s => r.service.to_lowercase() == s,
            };
            if !keep {
                continue;
            }
            let name = if r.creator_name.is_empty() { &r.user_id } else { &r.creator_name };
            keyed.push((if n > 0 { 0 } else { 1 }, name.to_lowercase(), i));
        }
        keyed.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        Built { shown: keyed.into_iter().map(|k| k.2).collect(), on_disk, total: self.rows.len(), updated, new_posts }
    }

    /// The artists whose folder exists (the whole watchlist; kept until the list changes)
    pub fn on_disk_count(&mut self) -> usize {
        if let Some(n) = self.on_disk_total {
            return n;
        }
        let mut disk = Disk::default();
        if !self.base_dir.is_empty() && Path::new(&self.base_dir).is_dir() {
            disk.subdirs = Some(self.base_subdirs());
        }
        let n = self.rows.iter().filter(|r| self.on_disk(r, &mut disk)).count();
        self.on_disk_total = Some(n);
        n
    }

    pub fn invalidate_disk(&mut self) {
        self.on_disk_total = None;
    }

    /// One row as the window reads it
    pub fn row(&self, i: usize, built: &Built) -> Value {
        let r = &self.rows[i];
        let is_disk = match built.on_disk.get(&i) {
            Some(d) => *d,
            None => self.on_disk(r, &mut Disk::default()),
        };
        json!({
            "url": r.url, "creatorName": r.creator_name, "service": r.service, "domain": r.domain, "userId": r.user_id,
            "lastPostDate": r.last_post_date, "lastPostId": r.last_post_id, "addedAt": r.added_at, "autoCheck": r.auto_check,
            "newPostCount": r.new_post_count, "downloadDir": r.download_dir, "ignoredCount": r.ignored_count,
            "cachedNewPosts": posts_for_window(r), "downloadDirs": download_dirs(r), "isFavorite": r.is_favorite,
            "provider": provider(r), "isOnDisk": is_disk, "display": r.creator_name,
        })
    }

    /// The model's get(index): the row's fields, as the Python model gave them
    pub fn get(&self, index: usize, built: &Built) -> Value {
        let Some(&i) = built.shown.get(index) else { return json!({}) };
        let mut v = self.row(i, built);
        if let Some(m) = v.as_object_mut() {
            m.remove("display");
        }
        v
    }

    pub fn props(&mut self) -> Value {
        let b = self.build();
        let auto = self.rows.iter().filter(|r| r.auto_check).count();
        let fav = self.rows.iter().filter(|r| r.is_favorite).count();
        json!({
            "count": b.shown.len(), "totalCount": b.total, "updatedCount": b.updated, "totalNewPosts": b.new_posts,
            "autoCheckCount": auto, "favoriteCount": fav, "onDiskCount": self.on_disk_count(),
        })
    }
}

pub fn global() -> &'static Mutex<View> {
    static V: OnceLock<Mutex<View>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(View::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// python tools/golden_watchlist_view.py
    #[test]
    fn matches_python_watchlist_model() {
        let fx: Value = serde_json::from_str(include_str!("../../tests/fixtures/watchlist_view.json")).unwrap();
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/target/wlview_base").replace('/', "\\");
        let _ = std::fs::remove_dir_all(&base);
        for d in fx["dirs"].as_array().unwrap() {
            std::fs::create_dir_all(format!("{base}\\{}", d.as_str().unwrap())).unwrap();
        }
        let sub = |v: &Value| -> Value { serde_json::from_str(&v.to_string().replace("{BASE}", &base.replace('\\', "\\\\"))).unwrap() };
        let rows: Vec<Row> = serde_json::from_value(sub(&fx["rows"])).unwrap();
        let mut v = View::new();
        v.base_dir = base.clone();
        v.include_service = fx["include_service"].as_bool().unwrap();
        v.apply(Some(rows.iter().map(|r| r.key.clone()).collect()), rows);
        let mut wrong = Vec::new();
        for c in fx["cases"].as_array().unwrap() {
            v.search = c["search"].as_str().unwrap().into();
            v.filter = c["filter"].as_str().unwrap().into();
            let b = v.build();
            let props = v.props();
            let got = json!({
                "rows": b.shown.iter().map(|&i| v.row(i, &b)).collect::<Vec<_>>(),
                "counts": [props["count"], props["totalCount"], props["updatedCount"], props["totalNewPosts"], props["autoCheckCount"], props["favoriteCount"], props["onDiskCount"]],
                "get0": v.get(0, &b),
            });
            let want = sub(&c["want"]);
            for k in ["rows", "counts", "get0"] {
                if got[k] != want[k] {
                    wrong.push(format!("{:?}/{:?} {k}\n  python: {}\n  rust:   {}", c["search"], c["filter"], want[k], got[k]));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&base);
        assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
    }
}
