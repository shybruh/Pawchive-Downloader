//! "decompressorBridge" and its "groupsModel": the Decompressor window (was bridge/decompressor_bridge.py).
//!
//! The archives found, the window's list (a header row per creator and folder, then its archives),
//! selecting, the scan, the disk check, extracting with a few archives at a time and the password
//! prompts, and the password bank's buttons. The engine itself is decompress/ (7-Zip, the bank). The
//! app tells what only it knows (the settings folder, where 7-Zip is, the watchlist's folders, the
//! saved settings: "decompressor_context") and saves the settings it's sent ("decompressor_settings").

use crate::decompress::{self, bank};
use crate::downloader::py::{basename, dirname, join, normcase, normpath};
use crate::downloader::service;
use crate::host::native::{Events, NativeObject, Prop};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const NAME: &str = "decompressorBridge";
pub const GROUPS: &str = "decompressorBridge.groupsModel";

type Item = Map<String, Value>;

fn s(m: &Item, k: &str) -> String {
    m.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn size(m: &Item) -> i64 {
    m.get("size").and_then(Value::as_i64).unwrap_or(0)
}

fn sel(m: &Item) -> bool {
    m.get("selected").and_then(Value::as_bool).unwrap_or(false)
}

fn exists(p: &str) -> bool {
    !p.is_empty() && Path::new(p).exists()
}

fn is_dir(p: &str) -> bool {
    !p.is_empty() && Path::new(p).is_dir()
}

/// The folder an archive's group is keyed by (the scanned folder, else its own)
fn raw_root(it: &Item) -> String {
    let r = s(it, "scanRoot");
    if r.is_empty() { s(it, "directory") } else { r }
}

fn norm(p: &str) -> String {
    normcase(&normpath(p))
}

/// _format_bytes
pub fn format_bytes(b: i64) -> String {
    if b <= 0 {
        return "0 B".into();
    }
    let mut b = b as f64;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if b < 1024.0 {
            return format!("{b:.1} {unit}");
        }
        b /= 1024.0;
    }
    format!("{b:.1} PB")
}

/// The run's speed and time left, as the window shows them
pub fn speed_eta(done: f64, total: f64, elapsed: f64) -> (String, String) {
    let elapsed = elapsed.max(0.1);
    let speed = done / elapsed;
    let rem = (total - done).max(0.0);
    let eta = if speed > 0.0 { rem / speed } else { 0.0 };
    let speed_str = if speed >= 1024.0 * 1024.0 { format!("{:.1} MB/s", speed / (1024.0 * 1024.0)) } else { format!("{:.0} KB/s", speed / 1024.0) };
    let eta_str = if eta > 3600.0 {
        format!("{}h {}m", (eta / 3600.0) as i64, ((eta % 3600.0) / 60.0) as i64)
    } else if eta > 60.0 {
        format!("{}m {}s", (eta / 60.0) as i64, (eta % 60.0) as i64)
    } else {
        format!("{}s", eta as i64)
    };
    (speed_str, eta_str)
}

/// What only the app knows
#[derive(Default, Clone)]
struct Ctx {
    config_dir: String,
    seven: String,
    vault: String,
    download_dir: String,
    watchlist: Vec<Value>,
    settings: Value,
}

fn context() -> Result<Ctx, String> {
    let v = service::ask_app("decompressor_context", json!([]))?;
    if !v.is_object() {
        return Err("no answer".into());
    }
    let st = |k: &str| v[k].as_str().unwrap_or("").to_string();
    Ok(Ctx {
        config_dir: st("config_dir"),
        seven: st("seven"),
        vault: st("vault"),
        download_dir: st("download_dir"),
        watchlist: v["watchlist"].as_array().cloned().unwrap_or_default(),
        settings: v["settings"].clone(),
    })
}

/// The folders scanned: the watchlist's (its folder, else the default folder or the creator's
/// folder in it), then the folders added here; each folder once
pub fn locations(watchlist: &[Value], download_dir: &str, custom: &[(String, String)]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for e in watchlist {
        let st = |k: &str| e[k].as_str().unwrap_or("").to_string();
        let (name, service_, user_id) = (st("creator_name"), st("service"), st("user_id"));
        let mut p = st("download_dir").trim().to_string();
        if p.is_empty() && exists(download_dir) {
            let cand = join(&[download_dir, &format!("{name} [{service_}]")]);
            p = if exists(&cand) { cand } else { download_dir.to_string() };
        }
        if !exists(&p) {
            continue;
        }
        if is_dir(&p) && !name.is_empty() {
            let lname = name.to_lowercase();
            let expected = [if service_.is_empty() { String::new() } else { format!("{name} [{service_}]").to_lowercase() }, lname.clone()];
            if let Ok(rd) = std::fs::read_dir(&p) {
                for d in rd.flatten() {
                    let sub = d.file_name().to_string_lossy().into_owned();
                    let low = sub.to_lowercase();
                    if expected.iter().any(|x| !x.is_empty() && *x == low) || (!service_.is_empty() && low.starts_with(&format!("{lname} ["))) {
                        let cand = join(&[&p, &sub]);
                        if is_dir(&cand) {
                            p = cand;
                            break;
                        }
                    }
                }
            }
        }
        out.push(json!({"path": p, "creator": if name.is_empty() { user_id } else { name }, "source": "watchlist"}));
    }
    for (path, creator) in custom {
        out.push(json!({"path": path, "creator": creator, "source": "custom"}));
    }
    let mut seen = HashSet::new();
    out.retain(|l| seen.insert(norm(l["path"].as_str().unwrap_or(""))));
    out
}

/// The window's list and its numbers
#[derive(Default)]
pub struct List {
    pub items: Vec<Item>,
    pub search: String,
    pub collapsed: HashSet<String>,
}

impl List {
    /// The archive as the window shows it (progress to one decimal)
    fn shown(it: &Item) -> Item {
        let mut m = it.clone();
        let p = it.get("progress").and_then(Value::as_f64).unwrap_or(0.0);
        m.insert("progress".into(), json!((p * 10.0).round() / 10.0));
        m
    }

    /// The groups shown (creator + folder), each with its archives
    pub fn groups(&self) -> Vec<(Item, Vec<Item>)> {
        let q = &self.search;
        let mut order: Vec<(String, String)> = Vec::new();
        let mut groups: HashMap<(String, String), (Item, Vec<Item>)> = HashMap::new();
        for it in &self.items {
            let c = { let c = s(it, "creator"); if c.is_empty() { "Unknown".to_string() } else { c } };
            if !q.is_empty() && !c.to_lowercase().contains(q) && !s(it, "filename").to_lowercase().contains(q)
                && !s(it, "directory").to_lowercase().contains(q) {
                continue;
            }
            let root = raw_root(it);
            let key = (c.clone(), if root.is_empty() { String::new() } else { norm(&root) });
            let g = groups.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                let m = json!({"creator": c, "directory": root, "groupKey": format!("{c}|{root}"), "totalCount": 0,
                               "selectedCount": 0, "totalBytes": 0, "selectedBytes": 0});
                (m.as_object().unwrap().clone(), Vec::new())
            });
            let add = |m: &mut Item, k: &str, n: i64| {
                let v = m[k].as_i64().unwrap_or(0) + n;
                m.insert(k.into(), json!(v));
            };
            add(&mut g.0, "totalCount", 1);
            add(&mut g.0, "totalBytes", size(it));
            if sel(it) {
                add(&mut g.0, "selectedCount", 1);
                add(&mut g.0, "selectedBytes", size(it));
            }
            g.1.push(Self::shown(it));
        }
        order.into_iter().map(|k| {
            let (mut g, items) = groups.remove(&k).unwrap();
            let (t, n) = (g["totalCount"].as_i64().unwrap_or(0), g["selectedCount"].as_i64().unwrap_or(0));
            g.insert("allSelected".into(), json!(t > 0 && n == t));
            g.insert("someSelected".into(), json!(n > 0 && n < t));
            (g, items)
        }).collect()
    }

    /// The rows: a group's header, then its archives unless it's folded; (rows, groups, any folded)
    pub fn rows(&self) -> (Vec<Value>, usize, bool) {
        let groups = self.groups();
        let n = groups.len();
        let mut rows = Vec::new();
        let mut any = false;
        for (mut g, items) in groups {
            let key = g["groupKey"].as_str().unwrap_or("").to_string();
            let folded = self.collapsed.contains(&key);
            any |= folded;
            g.insert("kind".into(), json!("group"));
            g.insert("rowKey".into(), json!(format!("g|{key}")));
            g.insert("collapsed".into(), json!(folded));
            rows.push(Value::Object(g));
            if !folded {
                for mut it in items {
                    let id = s(&it, "id");
                    it.insert("kind".into(), json!("item"));
                    it.insert("rowKey".into(), json!(format!("i|{id}")));
                    it.insert("groupKey".into(), json!(key));
                    rows.push(Value::Object(it));
                }
            }
        }
        (rows, n, any)
    }

    pub fn set_all_collapsed(&mut self, collapsed: bool) {
        self.collapsed = if collapsed { self.groups().into_iter().map(|(g, _)| g["groupKey"].as_str().unwrap_or("").to_string()).collect() } else { HashSet::new() };
    }

    pub fn toggle_collapsed(&mut self, key: &str) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_string());
        }
    }

    fn set_sel(it: &mut Item, v: bool) {
        it.insert("selected".into(), json!(v));
    }

    pub fn select_all(&mut self, v: bool) {
        self.items.iter_mut().for_each(|it| Self::set_sel(it, v));
    }

    pub fn select_creator(&mut self, creator: &str, v: bool) {
        self.items.iter_mut().filter(|it| s(it, "creator") == creator).for_each(|it| Self::set_sel(it, v));
    }

    fn in_folder(it: &Item, creator: &str, root: &str) -> bool {
        let want = if root.is_empty() { String::new() } else { norm(root) };
        s(it, "creator") == creator && norm(&raw_root(it)) == want
    }

    pub fn select_creator_by_root(&mut self, creator: &str, root: &str, v: bool) {
        self.items.iter_mut().filter(|it| Self::in_folder(it, creator, root)).for_each(|it| Self::set_sel(it, v));
    }

    fn toggle_where(&mut self, f: impl Fn(&Item) -> bool) {
        let all = { let mut m = self.items.iter().filter(|it| f(it)).peekable(); m.peek().is_some() && m.all(sel) };
        self.items.iter_mut().filter(|it| f(it)).for_each(|it| Self::set_sel(it, !all));
    }

    pub fn toggle_creator(&mut self, creator: &str) {
        self.toggle_where(|it| s(it, "creator") == creator);
    }

    pub fn toggle_creator_by_root(&mut self, creator: &str, root: &str) {
        self.toggle_where(|it| Self::in_folder(it, creator, root));
    }

    pub fn toggle_item(&mut self, id: &str) {
        if let Some(it) = self.items.iter_mut().find(|it| s(it, "id") == id) {
            let v = !sel(it);
            Self::set_sel(it, v);
        }
    }

    pub fn clear_finished(&mut self) {
        self.items.retain(|it| s(it, "status") != "done");
    }

    pub fn counts(&self) -> Value {
        let picked: Vec<&Item> = self.items.iter().filter(|it| sel(it)).collect();
        json!({"totalCount": self.items.len(), "selectedCount": picked.len(),
               "totalBytes": self.items.iter().map(|it| size(it)).sum::<i64>(),
               "selectedBytes": picked.iter().map(|it| size(it)).sum::<i64>()})
    }
}

#[derive(Clone)]
enum Answer {
    Submit(String, bool),
    Skip,
}

struct Settings {
    max_parallel: i64,
    threads: i64,
    delete_after: bool,
    auto_prompt: bool,
    header_collapsed: bool,
    custom: Vec<(String, String)>,
}

impl Settings {
    fn to_json(&self) -> Value {
        json!({"max_parallel": self.max_parallel, "threads_per_archive": self.threads, "delete_after": self.delete_after,
               "auto_prompt_passwords": self.auto_prompt, "header_collapsed": self.header_collapsed,
               "custom_locations": self.custom.iter().map(|(p, c)| json!({"path": p, "creator": c})).collect::<Vec<_>>()})
    }
}

fn int_of(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

pub struct Decompressor {
    events: Option<Events>,
    list: Mutex<List>,
    settings: Mutex<Settings>,
    ctx: Mutex<Option<Ctx>>,
    settings_from_app: AtomicBool,
    changed_here: AtomicBool,
    scanning: AtomicBool,
    extracting: AtomicBool,
    cancel: Arc<AtomicBool>,
    skip_all: AtomicBool,
    prompts: Mutex<HashMap<String, Option<Answer>>>,
    prompt_cv: Condvar,
    prompt_gate: Mutex<()>,
    locations_cache: Mutex<Vec<Value>>,
    rows: Mutex<(Vec<Value>, usize, bool)>,
    dirty: AtomicBool,
}

fn log(level: &str, msg: &str) {
    service::log_app(level, msg, "decompressor", "");
}

/// The password bank changed (here or by the app): the window's list of passwords follows
static BANK_TOUCHED: AtomicBool = AtomicBool::new(false);

pub fn bank_touched() {
    BANK_TOUCHED.store(true, Ordering::Relaxed);
}

impl Decompressor {
    pub fn new(events: Option<Events>) -> Arc<Self> {
        Arc::new(Self {
            events,
            list: Mutex::new(List::default()),
            settings: Mutex::new(Settings { max_parallel: 2, threads: 2, delete_after: false, auto_prompt: true, header_collapsed: false, custom: Vec::new() }),
            ctx: Mutex::new(None),
            settings_from_app: AtomicBool::new(false),
            changed_here: AtomicBool::new(false),
            scanning: AtomicBool::new(false),
            extracting: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            skip_all: AtomicBool::new(false),
            prompts: Mutex::new(HashMap::new()),
            prompt_cv: Condvar::new(),
            prompt_gate: Mutex::new(()),
            locations_cache: Mutex::new(Vec::new()),
            rows: Mutex::new((Vec::new(), 0, false)),
            dirty: AtomicBool::new(true),
        })
    }

    fn list(&self) -> MutexGuard<'_, List> {
        self.list.lock().unwrap()
    }

    fn changed(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn signal(&self, name: &str, args: Vec<Value>) {
        if let Some(e) = &self.events {
            e.signal(NAME, name, args);
        }
    }

    /// The app's details, asked once it's there (the saved settings taken the first time)
    fn ctx(&self) -> Option<Ctx> {
        let fresh = context();
        let mut c = self.ctx.lock().unwrap();
        match fresh {
            Ok(n) => {
                if !self.settings_from_app.swap(true, Ordering::Relaxed) && !self.changed_here.load(Ordering::Relaxed) {
                    self.load_settings(&n.settings);
                }
                *c = Some(n);
            }
            Err(e) if c.is_none() => {
                log("debug", &format!("The Decompressor couldn't ask the app for its folders: {e}"));
            }
            Err(_) => {}
        }
        c.clone()
    }

    fn cached_ctx(&self) -> Option<Ctx> {
        let c = self.ctx.lock().unwrap().clone();
        c.or_else(|| self.ctx())
    }

    fn load_settings(&self, d: &Value) {
        if !d.is_object() {
            return;
        }
        let mut guard = self.settings.lock().unwrap();
        let st = &mut *guard;
        if let Some(n) = int_of(&d["max_parallel"]) {
            st.max_parallel = n.clamp(1, 8);
        }
        if let Some(n) = int_of(&d["threads_per_archive"]) {
            st.threads = n.clamp(1, 16);
        }
        for (k, slot) in [("delete_after", &mut st.delete_after), ("auto_prompt_passwords", &mut st.auto_prompt), ("header_collapsed", &mut st.header_collapsed)] {
            if let Some(b) = d[k].as_bool() {
                *slot = b;
            }
        }
        st.custom = d["custom_locations"].as_array().into_iter().flatten()
            .filter(|c| c["path"].as_str().is_some_and(|p| !p.is_empty()))
            .map(|c| (c["path"].as_str().unwrap().to_string(), c["creator"].as_str().unwrap_or("Custom").to_string()))
            .collect();
        drop(guard);
        self.changed();
    }

    fn save_settings(&self) {
        self.changed_here.store(true, Ordering::Relaxed);
        let v = self.settings.lock().unwrap().to_json();
        service::note_app("decompressor_settings", v);
        self.changed();
    }

    fn locations(&self) -> Vec<Value> {
        let custom = self.settings.lock().unwrap().custom.clone();
        let locs = match self.cached_ctx() {
            Some(c) => locations(&c.watchlist, &c.download_dir, &custom),
            None => locations(&[], "", &custom),
        };
        *self.locations_cache.lock().unwrap() = locs.clone();
        locs
    }

    fn props(&self) -> Value {
        let st = self.settings.lock().unwrap();
        let (scanning, extracting) = (self.scanning.load(Ordering::Relaxed), self.extracting.load(Ordering::Relaxed));
        let mut p = self.list().counts();
        let r = self.rows.lock().unwrap();
        let seven = self.ctx.lock().unwrap().as_ref().map(|c| c.seven.clone());
        let passwords = self.bank("passwords", json!({})).unwrap_or(json!([]));
        let m = p.as_object_mut().unwrap();
        m.extend([
            ("isScanning".to_string(), json!(scanning)), ("isExtracting".into(), json!(extracting)), ("isBusy".into(), json!(scanning || extracting)),
            ("maxParallel".into(), json!(st.max_parallel)), ("threadsPerArchive".into(), json!(st.threads)),
            ("deleteAfter".into(), json!(st.delete_after)), ("autoPromptPasswords".into(), json!(st.auto_prompt)),
            ("headerCollapsed".into(), json!(st.header_collapsed)),
            ("groupCount".into(), json!(r.1)), ("anyCollapsed".into(), json!(r.2)),
            // (true until the app has said: the window doesn't warn before it knows)
            ("has7za".into(), json!(seven.map_or(true, |s| exists(&s)))),
            ("savedPasswordsCount".into(), json!(passwords.as_array().map_or(0, Vec::len))), ("savedPasswords".into(), passwords),
            ("locationsJson".into(), json!(serde_json::to_string(&*self.locations_cache.lock().unwrap()).unwrap_or_default())),
        ]);
        p
    }

    fn rebuild_rows(&self) -> usize {
        let r = self.list().rows();
        let n = r.0.len();
        *self.rows.lock().unwrap() = r;
        n
    }

    /// Tells the window what changed, a few times a second at most; asks the app for its details
    /// once it's there
    pub fn start_pump(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::Builder::new().name("decompressor-events".into()).spawn(move || {
            let mut last = Map::new();
            let mut known = false;
            loop {
                std::thread::sleep(Duration::from_millis(150));
                if !known && me.ctx().is_some() {
                    known = true;
                    me.locations();
                    me.changed();
                }
                let bank = BANK_TOUCHED.swap(false, Ordering::Relaxed);
                if !me.dirty.swap(false, Ordering::Relaxed) && !bank {
                    continue;
                }
                let n = me.rebuild_rows();
                let props = me.props();
                let changed: Map<String, Value> = props.as_object().unwrap().iter()
                    .filter(|(k, v)| last.get(*k) != Some(v)).map(|(k, v)| (k.clone(), v.clone())).collect();
                if let Some(e) = &me.events {
                    if !changed.is_empty() {
                        e.props(NAME, Value::Object(changed));
                    }
                    e.model(GROUPS, n);
                }
                last = props.as_object().unwrap().clone();
            }
        }).expect("decompressor events thread");
    }

    // ── The password bank ────────────────────────────────────────────────────

    fn bank(&self, op: &str, args: Value) -> Option<Value> {
        let c = self.ctx.lock().unwrap().clone()?;
        if c.config_dir.is_empty() {
            return None;
        }
        let mut a = args.as_object().cloned().unwrap_or_default();
        a.insert("dir".into(), json!(c.config_dir));
        a.insert("do".into(), json!(op));
        a.insert("vault".into(), json!(c.vault));
        bank::call(&Value::Object(a)).ok()
    }

    /// A change to the bank: the window and the app's copy follow
    fn bank_change(&self, op: &str, args: Value) -> Option<Value> {
        let v = self.bank(op, args);
        service::note_app("bank_changed", json!([]));
        self.signal("passwordBankChanged", vec![]);
        self.changed();
        v
    }

    // ── Scanning ─────────────────────────────────────────────────────────────

    pub fn scan_locations(self: &Arc<Self>) {
        if self.scanning.load(Ordering::Relaxed) || self.extracting.load(Ordering::Relaxed) {
            return;
        }
        self.scanning.store(true, Ordering::Relaxed);
        self.changed();
        self.signal("scanStarted", vec![]);
        let me = self.clone();
        std::thread::spawn(move || {
            me.ctx();
            let targets: Vec<(String, String)> = me.locations().iter()
                .map(|l| (l["path"].as_str().unwrap_or("").to_string(), l["creator"].as_str().unwrap_or("").to_string())).collect();
            log("info", &format!("Scanning {} location(s) for archives...", targets.len()));
            let found = decompress::scan(&targets, &AtomicBool::new(false));
            let items: Vec<Item> = found.into_iter().filter_map(|v| v.as_object().cloned()).collect();
            let total: i64 = items.iter().map(size).sum();
            let n = items.len();
            me.list().items = items;
            me.scanning.store(false, Ordering::Relaxed);
            me.changed();
            me.signal("itemsChanged", vec![]);
            me.signal("selectedStatsChanged", vec![]);
            me.signal("scanFinished", vec![json!(n), json!(total)]);
            log("info", &format!("Scan complete: found {n} archive(s) ({}).", format_bytes(total)));
        });
    }

    fn add_custom_folder(self: &Arc<Self>, folder: &str) {
        if !is_dir(folder) {
            return;
        }
        let n = norm(folder);
        let added = {
            let mut st = self.settings.lock().unwrap();
            if st.custom.iter().any(|(p, _)| norm(p) == n) {
                false
            } else {
                let name = basename(&normpath(folder)).to_string();
                st.custom.push((folder.to_string(), if name.is_empty() { "Custom".into() } else { name }));
                true
            }
        };
        if added {
            log("info", &format!("Added custom folder to decompressor: {folder}"));
            self.save_settings();
            self.locations();
        }
        self.scan_locations();
    }

    fn remove_custom(&self, f: impl Fn(usize, &str) -> bool) {
        let gone = {
            let mut st = self.settings.lock().unwrap();
            let before = st.custom.len();
            let mut i = 0;
            st.custom.retain(|(p, _)| {
                let keep = !f(i, p);
                i += 1;
                keep
            });
            st.custom.len() != before
        };
        if gone {
            self.save_settings();
            self.locations();
        }
    }

    // ── Extracting ───────────────────────────────────────────────────────────

    fn start(self: &Arc<Self>) {
        if self.extracting.load(Ordering::Relaxed) || self.scanning.load(Ordering::Relaxed) {
            return;
        }
        let (picked, done, pending) = {
            let l = self.list();
            let picked: Vec<&Item> = l.items.iter().filter(|it| sel(it)).collect();
            let is_done = |it: &&Item| s(it, "status") == "done" || it.get("extractedPresent").and_then(Value::as_bool).unwrap_or(false);
            let done = picked.iter().filter(|it| is_done(it)).count();
            let pending: Vec<String> = picked.iter().filter(|it| !is_done(it)).map(|it| s(it, "id")).collect();
            (picked.len(), done, pending)
        };
        if picked == 0 {
            return;
        }
        if pending.is_empty() && done > 0 {
            self.signal("redecompressConfirmRequested", vec![json!(done), json!(picked)]);
            return;
        }
        self.run(pending);
    }

    fn confirm_redo(self: &Arc<Self>) {
        if self.extracting.load(Ordering::Relaxed) || self.scanning.load(Ordering::Relaxed) {
            return;
        }
        let ids = {
            let mut l = self.list();
            for it in l.items.iter_mut().filter(|it| sel(it)) {
                if s(it, "status") == "done" || it.get("extractedPresent").and_then(Value::as_bool).unwrap_or(false) {
                    it.insert("status".into(), json!("pending"));
                    it.insert("progress".into(), json!(0.0));
                    it.insert("errorMessage".into(), json!(""));
                }
            }
            l.items.iter().filter(|it| sel(it)).map(|it| s(it, "id")).collect()
        };
        self.run(ids);
    }

    fn item(&self, id: &str) -> Option<Item> {
        self.list().items.iter().find(|it| s(it, "id") == id).cloned()
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Item)) {
        if let Some(it) = self.list().items.iter_mut().find(|it| s(it, "id") == id) {
            f(it);
        }
        self.changed();
    }

    fn set_status(&self, id: &str, status: &str, progress: Option<f64>, err: &str) {
        let mut shown = 0.0;
        self.update(id, |it| {
            it.insert("status".into(), json!(status));
            if let Some(p) = progress {
                it.insert("progress".into(), json!(p));
            }
            it.insert("errorMessage".into(), json!(err));
            shown = it.get("progress").and_then(Value::as_f64).unwrap_or(0.0);
        });
        self.signal("itemUpdated", vec![json!(id), json!(status), json!(shown), json!(err)]);
    }

    /// One extraction of an archive, the item kept up to date; (ok, error)
    fn extract(&self, id: &str, password: Option<&str>, delete_after: bool, cancel: &AtomicBool, progress: &mut dyn FnMut(f64)) -> (bool, String) {
        let Some(mut it) = self.item(id) else { return (false, "Archive no longer listed".into()) };
        let seven = self.cached_ctx().map(|c| c.seven).unwrap_or_default();
        let threads = self.settings.lock().unwrap().threads as u32;
        let (ok, err) = decompress::extract(&seven, &mut it, password, threads, delete_after, true, true, progress, cancel, &log);
        self.update(id, |cur| {
            let picked = cur.get("selected").cloned();
            *cur = it;
            if let Some(p) = picked {
                cur.insert("selected".into(), p);
            }
        });
        (ok, err)
    }

    fn test(&self, path: &str, password: &str, cancel: &AtomicBool) -> bool {
        let seven = self.cached_ctx().map(|c| c.seven).unwrap_or_default();
        decompress::test_password(&seven, path, password, cancel)
    }

    /// Shows one password prompt at a time and waits for its answer (five minutes at most)
    fn ask_password(&self, it: &Item, err: &str) -> Option<Answer> {
        let id = s(it, "id");
        let started = Instant::now();
        let gate = loop {
            if self.cancel.load(Ordering::Relaxed) || self.skip_all.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(310) {
                return None;
            }
            if let Ok(g) = self.prompt_gate.try_lock() {
                break g;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if self.cancel.load(Ordering::Relaxed) || self.skip_all.load(Ordering::Relaxed) {
            return None;
        }
        self.prompts.lock().unwrap().insert(id.clone(), None);
        self.signal("passwordPromptRequested", vec![json!(id), json!(s(it, "filename")), json!(s(it, "creator")),
                                                   json!(s(it, "directory")), json!(err), json!(size(it))]);
        let asked = Instant::now();
        let mut p = self.prompts.lock().unwrap();
        while p.get(&id).is_some_and(Option::is_none) && asked.elapsed() < Duration::from_secs(300) {
            p = self.prompt_cv.wait_timeout(p, Duration::from_millis(500)).unwrap().0;
        }
        let answer = p.remove(&id).flatten();
        drop(p);
        drop(gate);
        answer
    }

    fn answer(&self, id: &str, a: Answer) -> bool {
        let mut p = self.prompts.lock().unwrap();
        let waiting = p.contains_key(id);
        if waiting {
            p.insert(id.to_string(), Some(a));
            self.prompt_cv.notify_all();
        }
        waiting
    }

    fn record(&self, path: &str, password: &str, creator: &str) {
        self.bank_change("record", json!({"archive": path, "password": password, "creator": creator}));
    }

    /// One archive of a run: Some(true) extracted, Some(false) failed, None stopped
    fn process_one(&self, id: &str, run: &[String], total: f64, started: Instant) -> Option<bool> {
        let cancel = self.cancel.clone();
        let it = self.item(id)?;
        let (creator, filename) = (s(&it, "creator"), s(&it, "filename"));
        if cancel.load(Ordering::Relaxed) {
            self.set_status(id, "skipped", Some(0.0), "Cancelled");
            return None;
        }
        self.set_status(id, "extracting", Some(0.0), "");
        log("info", &format!("Extracting [{creator}] {filename}..."));
        let delete_after = self.settings.lock().unwrap().delete_after;
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut progress = |pct: f64| {
            self.update(id, |m| { m.insert("progress".into(), json!(pct)); });
            if pct < 100.0 && last.elapsed() < Duration::from_millis(100) {
                return;
            }
            last = Instant::now();
            self.signal("itemUpdated", vec![json!(id), json!("extracting"), json!(pct), json!("")]);
            let done: f64 = {
                let l = self.list();
                l.items.iter().filter(|m| run.contains(&s(m, "id"))).map(|m| {
                    let (sz, st) = (size(m) as f64, s(m, "status"));
                    match st.as_str() {
                        "done" | "compressing" => sz,
                        "extracting" => sz * m.get("progress").and_then(Value::as_f64).unwrap_or(0.0) / 100.0,
                        _ => 0.0,
                    }
                }).sum()
            };
            let t = total.max(1.0);
            let (speed, eta) = speed_eta(done, t, started.elapsed().as_secs_f64());
            self.signal("extractionProgress", vec![json!((done / t * 100.0).min(100.0)), json!(filename), json!(pct), json!(eta), json!(speed)]);
        };
        let first = Some(s(&it, "password")).filter(|p| !p.is_empty());
        let (mut ok, mut err) = self.extract(id, first.as_deref(), delete_after, &cancel, &mut progress);

        // Encrypted: the passwords the bank suggests, then the user
        if !ok && decompress::is_password_error(&err) && !cancel.load(Ordering::Relaxed) {
            log("info", &format!("Encrypted archive detected: [{creator}] {filename}. Testing candidate passwords..."));
            let path = s(&it, "path");
            let cands = self.bank("candidates", json!({"archive": path, "creator": creator})).unwrap_or(json!([]));
            let matched = cands.as_array().into_iter().flatten().filter_map(Value::as_str)
                .take_while(|_| !cancel.load(Ordering::Relaxed))
                .find(|c| self.test(&path, c, &cancel)).map(str::to_string);
            if let Some(pw) = matched {
                log("success", &format!("✓ Found matching password for [{creator}] {filename} (from the Password Bank)"));
                (ok, err) = self.extract(id, Some(&pw), delete_after, &cancel, &mut progress);
                if ok {
                    self.update(id, |m| { m.insert("password".into(), json!(pw)); });
                    self.record(&path, &pw, &creator);
                }
            }
        }
        if !ok && decompress::is_password_error(&err) && !cancel.load(Ordering::Relaxed)
            && self.settings.lock().unwrap().auto_prompt && !self.skip_all.load(Ordering::Relaxed) {
            if let Some(Answer::Submit(pw, remember)) = self.ask_password(&self.item(id).unwrap_or(it.clone()), &err) {
                if !pw.is_empty() {
                    (ok, err) = self.extract(id, Some(&pw), delete_after, &cancel, &mut progress);
                    if ok {
                        self.update(id, |m| { m.insert("password".into(), json!(pw)); });
                        if remember {
                            self.record(&s(&it, "path"), &pw, &creator);
                        }
                    }
                }
            }
        }
        if ok {
            let target = self.item(id).map(|m| s(&m, "targetDir")).unwrap_or_default();
            log("success", &format!("✓ Extracted [{creator}] {filename} -> {target}"));
            self.set_status(id, "done", Some(100.0), "");
            return Some(true);
        }
        if cancel.load(Ordering::Relaxed) && err.contains("Cancelled") {
            self.set_status(id, "skipped", Some(0.0), "Cancelled");
            return None;
        }
        if decompress::is_password_error(&err) {
            self.set_status(id, "password_required", Some(0.0), "Password required (encrypted archive)");
            log("warning", &format!("🔒 Password required for [{creator}] {filename}"));
        } else {
            self.set_status(id, "error", None, &err);
            log("error", &format!("✗ Error extracting [{creator}] {filename}: {err}"));
        }
        Some(false)
    }

    fn run(self: &Arc<Self>, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        self.extracting.store(true, Ordering::Relaxed);
        self.cancel.store(false, Ordering::Relaxed);
        self.skip_all.store(false, Ordering::Relaxed);
        self.changed();
        self.signal("extractionStarted", vec![]);
        let total: i64 = { let l = self.list(); l.items.iter().filter(|m| ids.contains(&s(m, "id"))).map(size).sum() };
        let (workers, threads, delete_after) = { let st = self.settings.lock().unwrap(); (st.max_parallel, st.threads, st.delete_after) };
        log("info", &format!("Starting bulk extraction: {} archive(s) ({}) | Workers: {workers}, Threads/archive: {threads}, Delete after: {}",
                             ids.len(), format_bytes(total), if delete_after { "True" } else { "False" }));
        let me = self.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let next = Mutex::new(0usize);
            let tally = Mutex::new((0usize, 0usize));
            std::thread::scope(|sc| {
                for _ in 0..workers.max(1) {
                    sc.spawn(|| loop {
                        let i = { let mut n = next.lock().unwrap(); *n += 1; *n - 1 };
                        let Some(id) = ids.get(i) else { break };
                        match me.process_one(id, &ids, total as f64, started) {
                            Some(true) => tally.lock().unwrap().0 += 1,
                            Some(false) => tally.lock().unwrap().1 += 1,
                            None => {}
                        }
                    });
                }
            });
            let (ok, failed) = *tally.lock().unwrap();
            me.extracting.store(false, Ordering::Relaxed);
            me.changed();
            me.signal("itemsChanged", vec![]);
            me.signal("selectedStatsChanged", vec![]);
            me.signal("extractionFinished", vec![json!(ok), json!(failed)]);
            log("success", &format!("Bulk decompression complete: {ok} succeeded, {failed} failed."));
        });
    }

    /// The window's "Unlock" on a row, or in a prompt that's waiting
    fn retry_with_password(self: &Arc<Self>, id: &str, password: &str, remember: bool) -> bool {
        let p = password.trim().to_string();
        let Some(it) = self.item(id) else { return false };
        let path = s(&it, "path");
        if !exists(&path) {
            return false;
        }
        if self.prompts.lock().unwrap().contains_key(id) {
            if !self.test(&path, &p, &AtomicBool::new(false)) {
                log("debug", &format!("Wrong password for {}", s(&it, "filename")));
                self.signal("passwordWrong", vec![json!(id)]);
                return false;
            }
            self.update(id, |m| { m.insert("password".into(), json!(p)); });
            if remember {
                self.record(&path, &p, "");
            }
            self.answer(id, Answer::Submit(p, remember));
            self.signal("passwordPromptDismissed", vec![json!(id)]);
            return true;
        }
        let me = self.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            let no = AtomicBool::new(false);
            if !me.test(&path, &p, &no) {
                log("debug", &format!("Wrong password for {}", s(&it, "filename")));
                me.signal("passwordWrong", vec![json!(id)]);
                return;
            }
            me.update(&id, |m| { m.insert("password".into(), json!(p)); });
            if remember {
                me.record(&path, &p, "");
            }
            me.set_status(&id, "extracting", Some(0.0), "");
            me.signal("passwordPromptDismissed", vec![json!(id)]);
            let delete_after = me.settings.lock().unwrap().delete_after;
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut progress = |pct: f64| {
                me.update(&id, |m| { m.insert("progress".into(), json!(pct)); });
                if pct < 100.0 && last.elapsed() < Duration::from_millis(100) {
                    return;
                }
                last = Instant::now();
                me.signal("itemUpdated", vec![json!(id), json!("extracting"), json!(pct), json!("")]);
            };
            let (ok, err) = me.extract(&id, Some(&p), delete_after, &no, &mut progress);
            if ok {
                me.set_status(&id, "done", Some(100.0), "");
                log("success", &format!("✓ Extracted [{}] {} with password", s(&it, "creator"), s(&it, "filename")));
            } else {
                me.set_status(&id, "error", None, &err);
            }
            me.signal("itemsChanged", vec![]);
        });
        true
    }

    fn cancel(&self) {
        log("warning", "Bulk decompression cancelled by user.");
        self.cancel.store(true, Ordering::Relaxed);
        decompress::cancel_all();
        let ids: Vec<String> = self.prompts.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.answer(&id, Answer::Skip);
        }
    }

    fn skip_all(&self) {
        self.skip_all.store(true, Ordering::Relaxed);
        let ids: Vec<String> = self.prompts.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.answer(&id, Answer::Skip);
            self.signal("passwordPromptDismissed", vec![json!(id)]);
        }
    }

    fn open_folder(&self, path: &str) {
        if path.is_empty() {
            return;
        }
        let target = if is_dir(path) { path.to_string() } else { dirname(path) };
        if exists(&target) {
            let _ = std::process::Command::new("explorer").arg(normpath(&target)).spawn();
        }
    }
}

fn arg(args: &[Value], i: usize) -> String {
    args.get(i).and_then(Value::as_str).unwrap_or("").to_string()
}

fn arg_bool(args: &[Value], i: usize, or: bool) -> bool {
    args.get(i).and_then(Value::as_bool).unwrap_or(or)
}

pub struct Object(pub Arc<Decompressor>);

const PROPS: &[(&str, &str, bool)] = &[
    ("isBusy", "bool", false), ("isScanning", "bool", false), ("isExtracting", "bool", false),
    ("totalCount", "int", false), ("selectedCount", "int", false), ("totalBytes", "qint64", false), ("selectedBytes", "qint64", false),
    ("maxParallel", "int", true), ("threadsPerArchive", "int", true), ("deleteAfter", "bool", true), ("autoPromptPasswords", "bool", true),
    ("headerCollapsed", "bool", true), ("savedPasswords", "QVariant", false), ("savedPasswordsCount", "int", false),
    ("locationsJson", "QString", false), ("has7za", "bool", false), ("groupCount", "int", false), ("anyCollapsed", "bool", false),
];

impl NativeObject for Object {
    fn props(&self) -> Vec<Prop> {
        PROPS.iter().map(|&(name, kind, writable)| Prop { name, kind, writable }).collect()
    }
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            ("scanLocations", vec![]), ("addCustomFolder", vec!["folder"]), ("removeCustomFolder", vec!["folder"]),
            ("removeCustomLocation", vec!["index"]), ("selectAll", vec!["selected"]), ("selectCreator", vec!["creatorName", "selected"]),
            ("selectCreatorByScanRoot", vec!["creatorName", "scanRoot", "selected"]), ("toggleCreator", vec!["creatorName"]),
            ("toggleCreatorByScanRoot", vec!["creatorName", "scanRoot"]), ("toggleItem", vec!["itemId"]),
            ("setSearch", vec!["text"]), ("toggleGroupCollapsed", vec!["key"]), ("setAllCollapsed", vec!["collapsed"]),
            ("checkDiskSpace", vec![]), ("startDecompression", vec![]), ("confirmRedecompress", vec![]),
            ("cancelDecompression", vec![]), ("clearFinished", vec![]), ("openFolder", vec!["path"]),
            ("getSavedPasswords", vec![]), ("addPassword", vec!["password", "creator"]), ("addMultiplePasswords", vec!["text"]),
            ("removePassword", vec!["password", "creator"]), ("clearSavedPasswords", vec![]), ("syncFromLinkVault", vec![]),
            ("setItemPassword", vec!["itemId", "password"]), ("testItemPassword", vec!["itemId", "password"]),
            ("submitPromptPassword", vec!["itemId", "password", "remember"]), ("skipPasswordPrompt", vec!["itemId"]),
            ("skipAllPasswordPrompts", vec![]), ("retryItemWithPassword", vec!["itemId", "password", "remember"]),
        ]
    }
    fn signals(&self) -> Vec<&'static str> {
        vec!["scanStarted", "scanFinished", "scanError", "itemsChanged", "selectedStatsChanged", "diskCheckCompleted",
             "extractionStarted", "extractionProgress", "itemUpdated", "extractionFinished", "redecompressConfirmRequested",
             "passwordPromptRequested", "passwordPromptDismissed", "passwordWrong", "passwordBankChanged"]
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        if prop == "locationsJson" {
            return Ok(json!(serde_json::to_string(&self.0.locations()).unwrap_or_default()));
        }
        self.0.rebuild_rows();
        self.0.props().get(prop).cloned().ok_or_else(|| format!("unknown property {prop:?}"))
    }
    fn set(&self, prop: &str, value: Value) -> Result<Value, String> {
        {
            let mut st = self.0.settings.lock().unwrap();
            match prop {
                "maxParallel" => st.max_parallel = int_of(&value).ok_or("not a number")?.clamp(1, 8),
                "threadsPerArchive" => st.threads = int_of(&value).ok_or("not a number")?.clamp(1, 16),
                "deleteAfter" => st.delete_after = value.as_bool().unwrap_or(false),
                "autoPromptPasswords" => st.auto_prompt = value.as_bool().unwrap_or(false),
                "headerCollapsed" => st.header_collapsed = value.as_bool().unwrap_or(false),
                p => return Err(format!("{p} is read-only")),
            }
        }
        self.0.save_settings();
        Ok(Value::Null)
    }
    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        let d = &self.0;
        let lists = |f: &dyn Fn(&mut List)| {
            f(&mut d.list());
            d.changed();
            d.signal("itemsChanged", vec![]);
            d.signal("selectedStatsChanged", vec![]);
        };
        Ok(match method {
            "scanLocations" => { d.scan_locations(); Value::Null }
            "addCustomFolder" => { d.add_custom_folder(&arg(&args, 0)); Value::Null }
            "removeCustomFolder" => {
                let n = norm(&arg(&args, 0));
                d.remove_custom(|_, p| norm(p) == n);
                Value::Null
            }
            "removeCustomLocation" => {
                let i = args.first().and_then(int_of).unwrap_or(-1);
                d.remove_custom(|j, _| j as i64 == i);
                Value::Null
            }
            "selectAll" => { lists(&|l| l.select_all(arg_bool(&args, 0, false))); Value::Null }
            "selectCreator" => { lists(&|l| l.select_creator(&arg(&args, 0), arg_bool(&args, 1, false))); Value::Null }
            "selectCreatorByScanRoot" => { lists(&|l| l.select_creator_by_root(&arg(&args, 0), &arg(&args, 1), arg_bool(&args, 2, false))); Value::Null }
            "toggleCreator" => { lists(&|l| l.toggle_creator(&arg(&args, 0))); Value::Null }
            "toggleCreatorByScanRoot" => { lists(&|l| l.toggle_creator_by_root(&arg(&args, 0), &arg(&args, 1))); Value::Null }
            "toggleItem" => { lists(&|l| l.toggle_item(&arg(&args, 0))); Value::Null }
            "clearFinished" => { lists(&|l| l.clear_finished()); Value::Null }
            "setSearch" => {
                d.list().search = arg(&args, 0).trim().to_lowercase();
                d.changed();
                Value::Null
            }
            "toggleGroupCollapsed" => { d.list().toggle_collapsed(&arg(&args, 0)); d.changed(); Value::Null }
            "setAllCollapsed" => { d.list().set_all_collapsed(arg_bool(&args, 0, false)); d.changed(); Value::Null }
            "checkDiskSpace" => {
                let items: Vec<Value> = d.list().items.iter().cloned().map(Value::Object).collect();
                let text = serde_json::to_string(&decompress::disk_check(&items)).unwrap_or_default();
                d.signal("diskCheckCompleted", vec![json!(text)]);
                json!(text)
            }
            "startDecompression" => { d.start(); Value::Null }
            "confirmRedecompress" => { d.confirm_redo(); Value::Null }
            "cancelDecompression" => { d.cancel(); Value::Null }
            "openFolder" => { d.open_folder(&arg(&args, 0)); Value::Null }
            "getSavedPasswords" => d.bank("passwords", json!({})).unwrap_or(json!([])),
            "addPassword" => {
                let creator = Some(arg(&args, 1)).filter(|c| !c.is_empty()).unwrap_or_else(|| "Global".into());
                let ok = d.bank("add", json!({"password": arg(&args, 0), "creator": creator})).and_then(|v| v.as_bool()).unwrap_or(false);
                if ok {
                    d.bank_change("passwords", json!({}));
                }
                json!(ok)
            }
            "addMultiplePasswords" => {
                let text = arg(&args, 0);
                let lines: Vec<Value> = text.replace(',', "\n").lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| json!(l)).collect();
                if lines.is_empty() {
                    return Ok(json!(0));
                }
                let n = d.bank("add_many", json!({"passwords": lines, "creator": "Global"})).and_then(|v| v.as_u64()).unwrap_or(0);
                if n > 0 {
                    d.bank_change("passwords", json!({}));
                }
                json!(n)
            }
            "removePassword" => {
                let ok = d.bank("remove", json!({"password": arg(&args, 0), "creator": arg(&args, 1)})).and_then(|v| v.as_bool()).unwrap_or(false);
                if ok {
                    d.bank_change("passwords", json!({}));
                }
                json!(ok)
            }
            "clearSavedPasswords" => {
                let ok = d.bank("clear", json!({})).and_then(|v| v.as_bool()).unwrap_or(false);
                if ok {
                    d.bank_change("passwords", json!({}));
                }
                json!(ok)
            }
            "syncFromLinkVault" => {
                let n = d.bank("sync", json!({})).and_then(|v| v.as_u64()).unwrap_or(0);
                if n > 0 {
                    d.bank_change("passwords", json!({}));
                }
                json!(n)
            }
            "setItemPassword" => {
                let p = arg(&args, 1).trim().to_string();
                d.update(&arg(&args, 0), |m| {
                    m.insert("password".into(), json!(p));
                    if s(m, "status") == "password_required" {
                        m.insert("status".into(), json!("pending"));
                        m.insert("errorMessage".into(), json!(""));
                    }
                });
                d.signal("itemsChanged", vec![]);
                Value::Null
            }
            "testItemPassword" => {
                let Some(it) = d.item(&arg(&args, 0)) else { return Ok(json!(false)) };
                let path = s(&it, "path");
                json!(exists(&path) && d.test(&path, arg(&args, 1).trim(), &AtomicBool::new(false)))
            }
            "submitPromptPassword" => {
                let id = arg(&args, 0);
                d.answer(&id, Answer::Submit(arg(&args, 1).trim().to_string(), arg_bool(&args, 2, true)));
                d.signal("passwordPromptDismissed", vec![json!(id)]);
                Value::Null
            }
            "skipPasswordPrompt" => {
                let id = arg(&args, 0);
                d.answer(&id, Answer::Skip);
                d.signal("passwordPromptDismissed", vec![json!(id)]);
                Value::Null
            }
            "skipAllPasswordPrompts" => { d.skip_all(); Value::Null }
            "retryItemWithPassword" => json!(d.retry_with_password(&arg(&args, 0), &arg(&args, 1), arg_bool(&args, 2, true))),
            m => return Err(format!("{NAME} has no method {m:?}")),
        })
    }
}

pub struct Groups(pub Arc<Decompressor>);

const ROLES: &[&str] = &["creator"];

impl NativeObject for Groups {
    fn props(&self) -> Vec<Prop> {
        vec![Prop { name: "count", kind: "int", writable: false }]
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        match prop {
            "count" => Ok(json!(self.0.rows.lock().unwrap().0.len())),
            _ => Err(format!("unknown property {prop:?}")),
        }
    }
    fn model_roles(&self) -> Option<&'static [&'static str]> {
        Some(ROLES)
    }
    fn rows(&self, start: usize, count: Option<usize>) -> Result<Value, String> {
        let r = self.0.rows.lock().unwrap();
        let total = r.0.len();
        let s = start.min(total);
        let e = count.map_or(total, |c| (s + c).min(total));
        let rows: Vec<Value> = r.0[s..e].iter().map(|row| json!({"creator": row})).collect();
        Ok(json!({"total": total, "start": s, "rows": rows}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/decompress_bridge.json")).unwrap()
    }

    fn items(fx: &Value) -> Vec<Item> {
        fx["items"].as_array().unwrap().iter().map(|v| v.as_object().unwrap().clone()).collect()
    }

    fn snapshot(l: &List) -> Value {
        let (rows, n, any) = l.rows();
        let mut c = l.counts();
        let m = c.as_object_mut().unwrap();
        m.insert("rows".into(), json!(rows));
        m.insert("groupCount".into(), json!(n));
        m.insert("anyCollapsed".into(), json!(any));
        c
    }

    fn check(got: &Value, want: &Value, what: &str) {
        for k in ["rows", "groupCount", "anyCollapsed", "totalCount", "selectedCount", "selectedBytes", "totalBytes"] {
            assert_eq!(got[k], want[k], "{what}: {k}");
        }
    }

    #[test]
    fn list_matches_python() {
        let fx = fixture();
        let mut l = List { items: items(&fx), ..Default::default() };
        for v in fx["views"].as_array().unwrap() {
            match v["collapsed"].as_str() {
                Some("all") => {
                    l.search.clear();
                    l.set_all_collapsed(true);
                }
                Some("toggled") => {
                    l.set_all_collapsed(false);
                    l.toggle_collapsed("Bob|c:\\DL\\bob\\\\");
                }
                _ => {
                    l.search = v["search"].as_str().unwrap().trim().to_lowercase();
                    l.collapsed = v["collapsed"].as_array().unwrap().iter().map(|k| k.as_str().unwrap().to_string()).collect();
                }
            }
            check(&snapshot(&l), v, &format!("view {:?} {:?}", v["search"], v["collapsed"]));
        }
    }

    #[test]
    fn selection_matches_python() {
        let fx = fixture();
        let mut l = List { items: items(&fx), ..Default::default() };
        for st in fx["selection"].as_array().unwrap() {
            let a = st["args"].as_array().unwrap();
            let t = |i: usize| a[i].as_str().unwrap().to_string();
            let b = |i: usize| a[i].as_bool().unwrap();
            match st["op"].as_str().unwrap() {
                "toggleItem" => l.toggle_item(&t(0)),
                "toggleCreatorByScanRoot" => l.toggle_creator_by_root(&t(0), &t(1)),
                "selectCreatorByScanRoot" => l.select_creator_by_root(&t(0), &t(1), b(2)),
                "selectCreator" => l.select_creator(&t(0), b(1)),
                "selectAll" => l.select_all(b(0)),
                "toggleCreator" => l.toggle_creator(&t(0)),
                "clearFinished" => l.clear_finished(),
                op => panic!("{op}"),
            }
            check(&snapshot(&l), st, &format!("{} {:?}", st["op"], a));
        }
    }

    #[test]
    fn locations_match_python() {
        let fx = fixture();
        let d = std::env::temp_dir().join(format!("pawchive_decbridge_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let base = normpath(&std::fs::canonicalize(&d).unwrap().to_string_lossy().replace(r"\\?\", ""));
        for sub in ["dl/Alice [patreon]", "dl/bob", "dl/Carol [fanbox]/x", "own/Dave", "own/dave [fantia]", "custom one", "Eve"] {
            std::fs::create_dir_all(Path::new(&base).join(sub.replace('/', "\\"))).unwrap();
        }
        std::fs::write(Path::new(&base).join("dl").join("Frank [patreon]"), "").unwrap();
        let b = |v: &Value| v.as_str().unwrap().replace("<B>", &base);
        let entries: Vec<Value> = fx["locations"]["entries"].as_array().unwrap().iter().map(|e| {
            let mut e = e.clone();
            e["download_dir"] = json!(b(&e["download_dir"]));
            e
        }).collect();
        for c in fx["locations"]["cases"].as_array().unwrap() {
            let custom: Vec<(String, String)> = c["custom"].as_array().unwrap().iter()
                .map(|p| { let p = b(p); (p.clone(), basename(&p).to_string()) }).collect();
            let got = locations(&entries, &b(&c["download_dir"]), &custom);
            let got: Value = serde_json::from_str(&serde_json::to_string(&got).unwrap().replace(&base.replace('\\', "\\\\"), "<B>")).unwrap();
            assert_eq!(got, c["locations"], "{}", c["name"]);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sizes_and_times() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(40), "40.0 B");
        assert_eq!(format_bytes(123456789012), "115.0 GB");
        assert_eq!(speed_eta(0.0, 100.0, 0.0), ("0 KB/s".to_string(), "0s".to_string()));
        assert_eq!(speed_eta(10.0 * 1048576.0, 100.0 * 1048576.0, 2.0), ("5.0 MB/s".to_string(), "18s".to_string()));
        assert_eq!(speed_eta(1024.0, 1024.0 * 1000.0, 1.0), ("1 KB/s".to_string(), "16m 39s".to_string()));
        assert_eq!(speed_eta(1.0, 10000.0, 1.0).1, "2h 46m");
    }
}
