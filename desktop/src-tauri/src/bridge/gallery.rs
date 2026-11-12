//! The Gallery's file explorer: folder listings (with folder sizes worked out in the background),
//! breadcrumbs, drives and their free space, moving and renaming, the duplicate finder, opening
//! and revealing files, and the safety check that keeps all of it away from system folders.
//!
//! A port of the "Integrated File Explorer & Media Gallery" part of bridge/app_bridge.py,
//! checked against it by tools/golden_gallery.py; also moving files to the Recycle Bin, opening a
//! folder and the other tabs' "Show in Gallery". The bookmarks (they are settings) are still the
//! Python bridge's.

use crate::downloader::py::{abspath, basename, commonpath, dirname, join, normcase, normpath, relpath, splitdrive, splitext};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What the explorer tells the window and the app (signals, log lines, the download folder)
pub trait Outside: Send + Sync {
    fn signal(&self, name: &str, args: Vec<Value>);
    fn log(&self, level: &str, message: &str, details: &str);
    fn download_dir(&self) -> String;
}

fn secs(t: std::io::Result<SystemTime>) -> f64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn home() -> String {
    std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into())
}

fn exists(p: &str) -> bool {
    fs::metadata(p).is_ok()
}

fn is_dir(p: &str) -> bool {
    fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

fn is_file(p: &str) -> bool {
    fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Files that changed (a download finished, files moved): the folders above them have new totals
static STALE: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn mark_stale(file_path: &str) {
    if !file_path.is_empty() {
        STALE.lock().unwrap().push(file_path.to_string());
    }
}

#[derive(Clone, Copy)]
struct FolderStats {
    size: u64,
    files: u64,
    dirs: u64,
    mtime: f64,
}

pub struct Gallery {
    out: Arc<dyn Outside>,
    stats_cache: Mutex<HashMap<String, FolderStats>>,
    stats_token: AtomicU64,
    last_total: AtomicU64,
    drive_cache: Mutex<HashMap<String, Map<String, Value>>>,
    drive_refresh_running: AtomicBool,
    drive_refresh_at: Mutex<Option<Instant>>,
    slow_drives: Mutex<HashMap<String, Instant>>,
    safety_cache: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Gallery {
    pub fn new(out: Arc<dyn Outside>) -> Arc<Self> {
        Arc::new(Self {
            out,
            stats_cache: Default::default(),
            stats_token: AtomicU64::new(0),
            last_total: AtomicU64::new(0),
            drive_cache: Default::default(),
            drive_refresh_running: AtomicBool::new(false),
            drive_refresh_at: Mutex::new(None),
            slow_drives: Default::default(),
            safety_cache: Default::default(),
        })
    }

    fn default_dir(&self) -> String {
        let d = self.out.download_dir();
        if d.is_empty() { home() } else { d }
    }

    // ── Folder listing ──────────────────────────────────────────────────────

    /// The folder's entries (folders first, then by name), at most `max_entries` of them. Folder
    /// sizes not known yet are -1 and follow with folderStatsCalculated.
    /// Forgets the totals of every folder above the files that changed (they're checked against a
    /// folder's own date, which doesn't change when files land in its subfolders)
    fn forget_stale(&self) {
        let stale: Vec<String> = std::mem::take(&mut *STALE.lock().unwrap());
        if stale.is_empty() {
            return;
        }
        let mut cache = self.stats_cache.lock().unwrap();
        for f in stale {
            let mut folder = normpath(&dirname(&f));
            loop {
                cache.remove(&folder);
                let parent = dirname(&folder);
                if parent == folder || parent.is_empty() {
                    break;
                }
                folder = parent;
            }
        }
    }

    pub fn list_directory(self: &Arc<Self>, path: &str, max_entries: usize) -> Vec<Value> {
        self.forget_stale();
        let path = if path.is_empty() { self.default_dir() } else { path.to_string() };
        self.last_total.store(0, Ordering::Relaxed);
        let path = abspath(&path);
        if !is_dir(&path) {
            return Vec::new();
        }
        let token = self.stats_token.fetch_add(1, Ordering::Relaxed) + 1;
        let Ok(rd) = fs::read_dir(&path) else { return Vec::new() };
        let mut items: Vec<(bool, String, Value)> = Vec::new();
        let mut to_scan = Vec::new();
        let (mut count, mut skipped) = (0u64, 0u64);
        for entry in rd {
            let Ok(entry) = entry else { continue };
            if count >= max_entries as u64 {
                skipped += 1;
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let norm_p = normpath(&join(&[&path, &name]));
            if ft.is_dir() {
                let mtime = secs(entry.metadata().and_then(|m| m.modified()));
                let cached = self.stats_cache.lock().unwrap().get(&norm_p).copied().filter(|c| c.mtime == mtime);
                let (size, files, dirs) = match cached {
                    Some(c) => (c.size as i64, c.files as i64, c.dirs as i64),
                    None => {
                        to_scan.push(norm_p.clone());
                        (-1, -1, -1)
                    }
                };
                items.push((true, name.to_lowercase(), json!({
                    "name": name, "path": norm_p, "is_dir": true, "size": size, "file_count": files,
                    "folder_count": dirs, "child_count": -1, "mtime": mtime, "ext": "",
                })));
            } else {
                let Ok(m) = entry.metadata() else { continue };
                let ext = splitext(&name).1.to_lowercase();
                items.push((false, name.to_lowercase(), json!({
                    "name": name, "path": norm_p, "is_dir": false, "size": m.len(), "file_count": 0,
                    "folder_count": 0, "child_count": 0, "mtime": secs(m.modified()), "ext": ext,
                })));
            }
            count += 1;
        }
        self.last_total.store(count + skipped, Ordering::Relaxed);
        if !to_scan.is_empty() {
            let me = self.clone();
            std::thread::spawn(move || {
                for p in to_scan {
                    if me.stats_token.load(Ordering::Relaxed) != token {
                        break;
                    }
                    me.folder_stats(&p, token);
                }
            });
        }
        items.sort_by(|a, b| (!a.0, &a.1).cmp(&(!b.0, &b.1)));
        items.into_iter().map(|i| i.2).collect()
    }

    pub fn last_listing_total(&self) -> u64 {
        self.last_total.load(Ordering::Relaxed)
    }

    /// A folder's size, files and subfolders (all levels, up to 250,000 files): told to the window
    fn folder_stats(&self, folder: &str, token: u64) {
        self.forget_stale();
        let Ok(meta) = fs::metadata(folder) else { return };
        if !meta.is_dir() {
            return;
        }
        let mtime = secs(meta.modified());
        let hit = self.stats_cache.lock().unwrap().get(folder).copied().filter(|c| c.mtime == mtime);
        if let Some(c) = hit {
            self.out.signal("folderStatsCalculated", vec![json!(normpath(folder)), json!(c.size), json!(c.files), json!(c.dirs)]);
            return;
        }
        let (mut size, mut files, mut dirs) = (0u64, 0u64, 0u64);
        // os.walk, top-down: a folder's files and subfolders, then each subfolder
        let mut stack = vec![folder.to_string()];
        'walk: while let Some(d) = stack.pop() {
            if self.stats_token.load(Ordering::Relaxed) != token {
                return;
            }
            let Ok(rd) = fs::read_dir(&d) else { continue };
            let mut sub = Vec::new();
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let p = e.path();
                let points_to_dir = ft.is_dir() || (ft.is_symlink() && p.is_dir());
                if points_to_dir {
                    dirs += 1;
                    if !ft.is_symlink() {
                        sub.push(p.to_string_lossy().into_owned());
                    }
                } else {
                    files += 1;
                    size += fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                }
            }
            if files >= 250_000 {
                break 'walk;
            }
            stack.extend(sub.into_iter().rev());
        }
        self.stats_cache.lock().unwrap().insert(folder.to_string(), FolderStats { size, files, dirs, mtime });
        if self.stats_token.load(Ordering::Relaxed) == token {
            self.out.signal("folderStatsCalculated", vec![json!(normpath(folder)), json!(size), json!(files), json!(dirs)]);
        }
    }

    pub fn breadcrumbs(&self, path: &str) -> Vec<Value> {
        let path = if path.is_empty() { self.default_dir() } else { path.to_string() };
        let mut curr = abspath(&path);
        let mut crumbs = Vec::new();
        while !curr.is_empty() {
            let parent = dirname(&curr);
            let name = basename(&curr);
            let name = if name.is_empty() { curr.clone() } else { name.to_string() };
            crumbs.push(json!({"name": name, "path": curr}));
            if parent == curr || parent.is_empty() {
                break;
            }
            curr = parent;
        }
        crumbs.reverse();
        crumbs
    }

    // ── Drives ──────────────────────────────────────────────────────────────

    fn no_space() -> Map<String, Value> {
        json!({"free_bytes": 0, "total_bytes": 0, "used_bytes": 0, "free_str": "", "total_str": "", "space_label": "", "percent_free": 100})
            .as_object()
            .unwrap()
            .clone()
    }

    pub fn drive_space_info(drive: &str) -> Map<String, Value> {
        let Some((total, free)) = disk_usage(drive) else { return Self::no_space() };
        let used = total.saturating_sub(free);
        let gb = 1024f64.powi(3);
        let (free_gb, total_gb) = (free as f64 / gb, total as f64 / gb);
        let fmt = |v: f64| {
            if v >= 1000.0 {
                format!("{:.1} TB", v / 1024.0)
            } else if v >= 10.0 {
                format!("{v:.0} GB")
            } else {
                format!("{v:.1} GB")
            }
        };
        let label = if total_gb >= 1000.0 && free_gb < 1000.0 {
            format!("{} / {}", fmt(free_gb), fmt(total_gb))
        } else {
            let n = |v: f64| if v >= 10.0 { format!("{v:.0}") } else { format!("{v:.1}") };
            format!("{}/{} GB", n(free_gb), n(total_gb))
        };
        let pct = if total > 0 { free as f64 / total as f64 * 100.0 } else { 0.0 };
        json!({"free_bytes": free, "total_bytes": total, "used_bytes": used, "free_str": fmt(free_gb), "total_str": fmt(total_gb),
               "space_label": label, "percent_free": pct})
        .as_object()
        .unwrap()
        .clone()
    }

    fn drive_item(&self, name: &str, path: &str) -> Value {
        let mut item = Map::new();
        item.insert("name".into(), json!(name));
        item.insert("path".into(), json!(path));
        let space = self.drive_cache.lock().unwrap().get(path).cloned().unwrap_or_else(Self::no_space);
        item.extend(space);
        Value::Object(item)
    }

    /// The drives with their last known free space (refreshed in the background: drivesUpdated)
    pub fn system_drives(self: &Arc<Self>) -> Vec<Value> {
        let roots = drive_roots();
        let drives: Vec<Value> = roots.iter().map(|(n, p)| self.drive_item(n, p)).collect();
        self.refresh_drive_space(roots);
        drives
    }

    fn refresh_drive_space(self: &Arc<Self>, roots: Vec<(String, String)>) {
        {
            let mut at = self.drive_refresh_at.lock().unwrap();
            if self.drive_refresh_running.load(Ordering::Relaxed) || at.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
                return;
            }
            *at = Some(Instant::now());
        }
        self.drive_refresh_running.store(true, Ordering::Relaxed);
        let me = self.clone();
        std::thread::spawn(move || {
            // An offline network drive can take a network timeout to answer: each drive is asked
            // on its own thread, and one that doesn't answer in 4 s is left alone for a minute
            let results: Arc<Mutex<HashMap<String, Map<String, Value>>>> = Arc::default();
            let mut probes = Vec::new();
            for (_, path) in &roots {
                if me.slow_drives.lock().unwrap().get(path).is_some_and(|t| Instant::now() < *t) {
                    continue;
                }
                let (r, p) = (results.clone(), path.clone());
                probes.push((path.clone(), std::thread::spawn(move || {
                    let info = Gallery::drive_space_info(&p);
                    r.lock().unwrap().insert(p, info);
                })));
            }
            let deadline = Instant::now() + Duration::from_secs(4);
            for (path, h) in probes {
                while !h.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if !h.is_finished() {
                    me.slow_drives.lock().unwrap().insert(path, Instant::now() + Duration::from_secs(60));
                }
            }
            me.drive_cache.lock().unwrap().extend(results.lock().unwrap().drain());
            let updated: Vec<Value> = roots.iter().map(|(n, p)| me.drive_item(n, p)).collect();
            me.out.signal("drivesUpdated", vec![json!(updated)]);
            me.drive_refresh_running.store(false, Ordering::Relaxed);
        });
    }

    // ── Opening ─────────────────────────────────────────────────────────────

    pub fn open_path_in_system(&self, path: &str) {
        let path = if path.is_empty() { self.out.download_dir() } else { path.to_string() };
        if !exists(&path) {
            return;
        }
        // (a folder opens in Explorer, a file in its program: Explorer does both)
        if let Err(e) = std::process::Command::new("explorer").arg(normpath(&path)).spawn() {
            if !is_dir(&path) {
                self.out.log("warning", &format!("Could not open file in system: {e}"), "");
            }
        }
    }

    pub fn reveal(&self, file_path: &str) -> bool {
        if file_path.is_empty() {
            return false;
        }
        let mut clean = normpath(&file_path.replace("file:///", "").replace("file://", ""));
        if !exists(&clean) {
            let d = dirname(&clean);
            if !exists(&d) {
                return false;
            }
            clean = d;
        }
        let mut cmd = std::process::Command::new("explorer");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            if is_file(&clean) {
                cmd.raw_arg(format!("/select,\"{clean}\""));
            } else {
                cmd.raw_arg(format!("\"{clean}\""));
            }
        }
        #[cfg(not(windows))]
        cmd.arg(&clean);
        match cmd.spawn() {
            Ok(_) => true,
            Err(e) => {
                self.out.log("error", &format!("Failed to reveal file {file_path}: {e}"), "");
                false
            }
        }
    }

    // ── Moving and renaming ─────────────────────────────────────────────────

    fn blocked_or_root(&self, p: &str) -> bool {
        let s = self.path_safety(p);
        s["is_blocked"].as_bool().unwrap_or(true) || s["is_drive_root"].as_bool().unwrap_or(false)
    }

    /// Moves files / folders into dest_dir; never overwrites. "pairs" lets the window undo it.
    pub fn move_items(&self, paths: &[String], dest_dir: &str) -> Value {
        let dest = if dest_dir.is_empty() { String::new() } else { normpath(dest_dir) };
        if dest.is_empty() || !is_dir(&dest) {
            return json!({"moved": 0, "failed": paths.len(), "errors": [format!("Destination folder does not exist: {dest_dir}")], "pairs": []});
        }
        if self.path_safety(&dest)["is_blocked"].as_bool().unwrap_or(true) {
            return json!({"moved": 0, "failed": paths.len(), "errors": [format!("Moving into a protected system folder is blocked: {dest}")], "pairs": []});
        }
        let dest_case = normcase(&dest);
        let (mut moved, mut failed) = (0, 0);
        let (mut errors, mut pairs): (Vec<String>, Vec<Value>) = (Vec::new(), Vec::new());
        for p in paths.iter().filter(|p| !p.is_empty()) {
            let src = normpath(p);
            let name = basename(&src).to_string();
            if self.blocked_or_root(&src) {
                failed += 1;
                errors.push(format!("Move blocked for system or root directory: {src}"));
                continue;
            }
            if !exists(&src) {
                failed += 1;
                errors.push(format!("Target does not exist: {src}"));
                continue;
            }
            if normcase(&dirname(&src)) == dest_case {
                continue; // already there
            }
            let src_case = normcase(&src);
            if dest_case == src_case || dest_case.starts_with(&format!("{src_case}\\")) {
                failed += 1;
                errors.push(format!("Cannot move a folder into itself: {name}"));
                continue;
            }
            let target = join(&[&dest, &name]);
            if exists(&target) {
                failed += 1;
                errors.push(format!("An item named '{name}' already exists in the destination"));
                continue;
            }
            match move_path(&src, &target) {
                Ok(()) => {
                    moved += 1;
                    pairs.push(json!([src, target]));
                }
                Err(e) => {
                    failed += 1;
                    errors.push(format!("Failed moving {name}: {e}"));
                }
            }
        }
        if moved > 0 {
            let details: Vec<String> = pairs.iter().map(|p| format!("{}  ->  {}", p[0].as_str().unwrap_or(""), p[1].as_str().unwrap_or(""))).collect();
            self.out.log("info", &format!("Moved {moved} item(s) to '{dest}'."), &details.join("\n"));
        }
        if !errors.is_empty() {
            self.out.log("warning", &format!("{failed} item(s) couldn't be moved to '{dest}'."), &errors.join("\n"));
        }
        json!({"moved": moved, "failed": failed, "errors": errors, "pairs": pairs})
    }

    pub fn rename_item(&self, path: &str, new_name: &str) -> Value {
        let fail = |e: &str| json!({"success": false, "error": e, "new_path": ""});
        let src = if path.is_empty() { String::new() } else { normpath(path) };
        let new_name = new_name.trim();
        if src.is_empty() || !exists(&src) {
            return fail("The item no longer exists.");
        }
        if new_name.is_empty() || new_name == "." || new_name == ".." || new_name.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*']) || new_name.ends_with(['.', ' ']) {
            return fail("That name contains characters that aren't allowed.");
        }
        if self.blocked_or_root(&src) {
            return fail("Renaming is blocked for system or root directories.");
        }
        let dst = join(&[&dirname(&src), new_name]);
        if dst == src {
            return json!({"success": true, "error": "", "new_path": src});
        }
        // A case-only rename points at the same file on Windows, so it is not a collision
        if exists(&dst) && normcase(&dst) != normcase(&src) {
            return fail(&format!("An item named '{new_name}' already exists here."));
        }
        let old = basename(&src).to_string();
        if let Err(e) = fs::rename(&src, &dst) {
            self.out.log("warning", &format!("Couldn't rename '{old}' to '{new_name}': {e}"), &src);
            return fail(&e.to_string());
        }
        self.out.log("info", &format!("Renamed '{old}' to '{new_name}'."), &format!("{src}  ->  {dst}"));
        json!({"success": true, "error": "", "new_path": dst})
    }

    // ── Duplicates ──────────────────────────────────────────────────────────

    /// Groups of identical files (same size, same start, same SHA-256), most space wasted first
    pub fn scan_duplicates(&self, folder: &str, recursive: bool) -> Vec<Value> {
        if folder.is_empty() || !exists(folder) {
            return Vec::new();
        }
        if self.path_safety(folder)["is_blocked"].as_bool().unwrap_or(true) {
            self.out.log("warning", &format!("scanDuplicates blocked on {folder} for system safety"), "");
            return Vec::new();
        }
        let root = abspath(folder);
        // Files by size, in the order they were found (Python dicts keep insertion order)
        let mut sizes: Vec<(u64, Vec<String>)> = Vec::new();
        let mut size_at: HashMap<u64, usize> = HashMap::new();
        let mut add = |p: String| {
            if let Ok(m) = fs::metadata(&p) {
                let sz = m.len();
                if sz > 0 {
                    let i = *size_at.entry(sz).or_insert_with(|| {
                        sizes.push((sz, Vec::new()));
                        sizes.len() - 1
                    });
                    sizes[i].1.push(p);
                }
            }
        };
        if recursive {
            let mut stack = vec![root.clone()];
            while let Some(d) = stack.pop() {
                let Ok(rd) = fs::read_dir(&d) else { continue };
                let mut sub = Vec::new();
                for e in rd.flatten() {
                    let Ok(ft) = e.file_type() else { continue };
                    let p = join(&[&d, &e.file_name().to_string_lossy()]);
                    if ft.is_dir() || (ft.is_symlink() && is_dir(&p)) {
                        if !ft.is_symlink() {
                            sub.push(p);
                        }
                    } else {
                        add(p);
                    }
                }
                stack.extend(sub.into_iter().rev());
            }
        } else if let Ok(rd) = fs::read_dir(&root) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    add(join(&[&root, &e.file_name().to_string_lossy()]));
                }
            }
        }
        // Same size and same first 64 KB…
        let mut partial: Vec<(u64, Vec<String>)> = Vec::new();
        let mut partial_at: HashMap<(u64, [u8; 16]), usize> = HashMap::new();
        for (sz, list) in &sizes {
            if list.len() < 2 {
                continue;
            }
            for p in list {
                let Ok(mut f) = fs::File::open(p) else { continue };
                let mut head = vec![0u8; 65536];
                let mut n = 0;
                while n < head.len() {
                    match f.read(&mut head[n..]) {
                        Ok(0) => break,
                        Ok(k) => n += k,
                        Err(_) => break,
                    }
                }
                let key = (*sz, md5::Md5::digest(&head[..n]).into());
                let i = *partial_at.entry(key).or_insert_with(|| {
                    partial.push((*sz, Vec::new()));
                    partial.len() - 1
                });
                partial[i].1.push(p.clone());
            }
        }
        // …then the same SHA-256
        let mut groups = Vec::new();
        for (sz, list) in &partial {
            if list.len() < 2 {
                continue;
            }
            let mut full: Vec<(String, Vec<String>)> = Vec::new();
            for p in list {
                let Some(h) = sha256_file(p) else { continue };
                match full.iter_mut().find(|(k, _)| *k == h) {
                    Some((_, v)) => v.push(p.clone()),
                    None => full.push((h, vec![p.clone()])),
                }
            }
            for (h, matches) in full {
                if matches.len() < 2 {
                    continue;
                }
                let mut files: Vec<(f64, Value)> = matches
                    .iter()
                    .map(|mp| {
                        let mtime = fs::metadata(mp).map(|m| secs(m.modified())).unwrap_or(0.0);
                        let rel = relpath(mp, &root).unwrap_or_else(|| mp.clone());
                        (mtime, json!({"path": normpath(mp), "name": basename(mp), "rel_path": rel, "mtime": mtime}))
                    })
                    .collect();
                files.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                let count = files.len() as u64;
                groups.push(json!({
                    "hash": &h[..12], "size": sz, "count": count, "wasted_size": (count - 1) * sz,
                    "files": files.into_iter().map(|f| f.1).collect::<Vec<_>>(),
                }));
            }
        }
        // (stable, like Python's sort)
        groups.sort_by(|a, b| b["wasted_size"].as_u64().cmp(&a["wasted_size"].as_u64()));
        groups
    }

    /// scanDuplicates on a background thread: the answer comes with asyncResultReady
    pub fn scan_duplicates_async(self: &Arc<Self>, request_id: &str, folder: &str, recursive: bool) {
        let (me, id, folder) = (self.clone(), request_id.to_string(), folder.to_string());
        std::thread::spawn(move || {
            let r = me.scan_duplicates(&folder, recursive);
            me.out.signal("asyncResultReady", vec![json!(id), json!(r)]);
        });
    }

    // ── Safety ──────────────────────────────────────────────────────────────

    /// Whether bulk operations (move, rename, delete, sort…) may touch this path. Answers are
    /// kept 30 s (the window asks on every folder change, and the check reads the disk).
    /// The download folder (else the home folder)
    pub fn download_dir_or_home(&self) -> String {
        let d = self.out.download_dir();
        if d.is_empty() { home() } else { d }
    }

    /// Opens a folder (a file's folder) in Explorer; a missing folder is made, else the download folder
    pub fn open_folder(&self, path: &str) {
        let dl = self.out.download_dir();
        let path = if path.is_empty() { dl.clone() } else { path.to_string() };
        let mut target = if is_dir(&path) { path.clone() } else { dirname(&path) };
        if !exists(&target) && fs::create_dir_all(&target).is_err() {
            target = dl;
        }
        if exists(&target) {
            let _ = std::process::Command::new("explorer").arg(normpath(&target)).spawn();
        }
    }

    /// Moves files or folders to the Recycle Bin (never deleted for good: what can't go there is
    /// reported). {"deleted", "failed", "errors"}
    pub fn delete_items(&self, paths: &[String]) -> Value {
        let (mut deleted, mut failed) = (0, 0);
        let (mut errors, mut trashed) = (Vec::new(), Vec::new());
        for p in paths.iter().filter(|p| !p.is_empty()) {
            let norm = normpath(p);
            let safety = self.path_safety(&norm);
            if safety["is_blocked"].as_bool().unwrap_or(false) || safety["is_drive_root"].as_bool().unwrap_or(false) {
                failed += 1;
                errors.push(format!("Deletion blocked for system or root directory: {norm}"));
                continue;
            }
            if !exists(&norm) {
                failed += 1;
                errors.push(format!("Target does not exist: {norm}"));
                continue;
            }
            if crate::decompress::to_recycle_bin(&norm) {
                deleted += 1;
                mark_stale(&join(&[&norm, "x"]));
                trashed.push(norm);
            } else {
                failed += 1;
                errors.push(format!("Could not move {} to the Recycle Bin", basename(&norm)));
            }
        }
        if deleted > 0 {
            self.out.log("info", &format!("Moved {deleted} item(s) to the Recycle Bin."), &trashed.join("\n"));
        }
        if !errors.is_empty() {
            self.out.log("warning", &format!("{failed} item(s) couldn't be moved to the Recycle Bin."), &errors.join("\n"));
        }
        json!({"deleted": deleted, "failed": failed, "errors": errors})
    }

    /// Asks the Gallery tab to open this folder (or the folder of this file, highlighted); a missing
    /// file: the closest folder that's there
    pub fn show_in_gallery(&self, path: &str) {
        if path.is_empty() {
            return;
        }
        let norm = normpath(path);
        if is_dir(&norm) {
            self.out.signal("galleryShowRequested", vec![json!(norm), json!(true)]);
        } else if exists(&norm) {
            self.out.signal("galleryShowRequested", vec![json!(norm), json!(false)]);
        } else {
            let mut parent = dirname(&norm);
            while !parent.is_empty() && !is_dir(&parent) && dirname(&parent) != parent {
                parent = dirname(&parent);
            }
            if !parent.is_empty() && is_dir(&parent) {
                self.out.signal("galleryShowRequested", vec![json!(parent), json!(true)]);
            }
        }
    }

    /// The Gallery at the folder of the queue's files, else the download folder
    pub fn show_downloads(&self, targets: &[String]) {
        let dirs: Vec<String> = targets.iter().filter(|t| !t.is_empty()).map(|t| dirname(t)).collect();
        let mut folder = if dirs.is_empty() { String::new() } else { commonpath(&dirs).unwrap_or_default() };
        if folder.is_empty() || !is_dir(&folder) {
            folder = self.download_dir_or_home();
        }
        self.show_in_gallery(&folder);
    }

    pub fn path_safety(&self, path: &str) -> Value {
        if let Some((t, v)) = self.safety_cache.lock().unwrap().get(path) {
            if t.elapsed() < Duration::from_secs(30) {
                return v.clone();
            }
        }
        let info = path_safety_info(path);
        let mut c = self.safety_cache.lock().unwrap();
        if c.len() > 500 {
            c.clear();
        }
        c.insert(path.to_string(), (Instant::now(), info.clone()));
        info
    }
}

fn sha256_file(p: &str) -> Option<String> {
    let mut f = fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// shutil.move: a rename, or copy and delete across drives
pub(crate) fn move_path(src: &str, dst: &str) -> std::io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(17) => {
            // ERROR_NOT_SAME_DEVICE
            if is_dir(src) {
                copy_tree(src, dst)?;
                fs::remove_dir_all(src)
            } else {
                fs::copy(src, dst)?;
                fs::remove_file(src)
            }
        }
        Err(e) => Err(e),
    }
}

fn copy_tree(src: &str, dst: &str) -> std::io::Result<()> {
    fs::create_dir(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let (s, d) = (join(&[src, &name]), join(&[dst, &name]));
        if e.file_type()?.is_dir() {
            copy_tree(&s, &d)?;
        } else {
            fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

/// (total, free) bytes of the drive holding `path`, like shutil.disk_usage
#[cfg(windows)]
fn disk_usage(path: &str) -> Option<(u64, u64)> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((total, free))
}

#[cfg(not(windows))]
fn disk_usage(_path: &str) -> Option<(u64, u64)> {
    None
}

/// [(name, path)] of the drives to show (nothing is opened)
fn drive_roots() -> Vec<(String, String)> {
    #[cfg(windows)]
    {
        let mut mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        if mask == 0 {
            return vec![("C:".into(), "C:\\".into())];
        }
        let mut out = Vec::new();
        for letter in b'A'..=b'Z' {
            if mask & 1 == 1 {
                out.push((format!("{}:", letter as char), format!("{}:\\", letter as char)));
            }
            mask >>= 1;
        }
        out
    }
    #[cfg(not(windows))]
    {
        vec![("Root (/)".into(), "/".into()), ("Home (~)".into(), home())]
    }
}

/// The checks behind Gallery.path_safety (Windows rules; the app runs on Windows)
pub fn path_safety_info(path: &str) -> Value {
    let windir = std::env::var("WINDIR").or_else(|_| std::env::var("SystemRoot")).unwrap_or_else(|_| "C:\\Windows".into());
    let sys_env = std::env::var("SystemDrive").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        let d = splitdrive(&windir).0;
        if d.is_empty() { "C:".into() } else { d.to_string() }
    });
    let system_drive = format!("{}:", sys_env.to_uppercase().trim_end_matches(':'));
    if path.trim().is_empty() {
        return json!({"is_blocked": true, "is_system_root": false, "is_system_dir": false, "is_drive_root": false, "is_other_root": false,
                      "system_drive": "C:", "drive_letter": "", "path": "", "message": "Empty or invalid folder path."});
    }
    let norm = abspath(path.trim());
    let norm_case = normcase(&norm);
    let sys_root = format!("{system_drive}\\");
    let norm_sys_root = normcase(&normpath(&sys_root));
    let (drive, tail) = splitdrive(&norm);
    let drive_letter = drive.to_uppercase();
    let tail_stripped = tail.trim_matches(['\\', '/']);
    let is_drive_root = !drive_letter.is_empty() && tail_stripped.is_empty();
    let is_system_root = is_drive_root && drive_letter == system_drive;
    let answer = |blocked: bool, sys_root_: bool, sys_dir: bool, drive_root: bool, other_root: bool, message: &str| {
        json!({"is_blocked": blocked, "is_system_root": sys_root_, "is_system_dir": sys_dir, "is_drive_root": drive_root,
               "is_other_root": other_root, "system_drive": system_drive, "drive_letter": drive_letter, "path": norm, "message": message})
    };
    let blocked = |message: &str| answer(true, false, true, false, false, message);
    let under = |base: &str| {
        let b = normcase(&normpath(base));
        norm_case == b || norm_case.starts_with(&format!("{b}\\"))
    };

    if is_system_root {
        return answer(true, true, false, true, false, &format!(
            "Operations are permanently disabled on the operating system drive root ({drive_letter}\\) for system stability."
        ));
    }
    // 2. Windows' own folders
    let protected = [
        windir.clone(),
        join(&[&sys_root, "Windows"]),
        join(&[&sys_root, "System Volume Information"]),
        join(&[&sys_root, "$Recycle.Bin"]),
        join(&[&sys_root, "Recovery"]),
        join(&[&sys_root, "PerfLogs"]),
        join(&[&sys_root, "Boot"]),
        join(&[&sys_root, "Documents and Settings"]),
        join(&[&sys_root, "MSOCache"]),
        join(&[&sys_root, "Config.Msi"]),
        join(&[&sys_root, "inetpub"]),
    ];
    if protected.iter().any(|d| under(d)) {
        return blocked("Operations are permanently disabled on protected operating system directories.");
    }
    if norm_case.starts_with(&norm_sys_root) && norm_case.split('\\').any(|p| p.starts_with('$')) {
        return blocked("Operations are permanently disabled on Windows system cache/recovery directories.");
    }
    // 3. Program Files & ProgramData (any drive)
    let parts: Vec<&str> = norm_case.split('\\').collect();
    if parts.iter().any(|p| matches!(*p, "program files" | "program files (x86)" | "programdata" | "windowsapps")) {
        return blocked("Operations are permanently disabled inside Program Files / ProgramData to protect installed software.");
    }
    // 4. Users and profiles
    let users_case = normcase(&join(&[&sys_root, "Users"]));
    if norm_case == users_case {
        return blocked(&format!(
            "Operations are permanently disabled on the Users directory ({drive_letter}\\Users) to protect user account data."
        ));
    }
    if normcase(&dirname(&norm)) == users_case {
        return blocked("Operations are permanently disabled directly on your User Profile home folder to protect profile configuration and system directories. Please select a subfolder (such as Downloads or Pictures).");
    }
    let temp_case = normcase(&normpath(&std::env::temp_dir().to_string_lossy()));
    let temp_case = temp_case.trim_end_matches('\\').to_string();
    let is_temp = norm_case == temp_case || norm_case.starts_with(&format!("{temp_case}\\"));
    if !is_temp && parts.iter().any(|p| matches!(*p, "appdata" | "application data" | "local settings")) {
        return blocked("Operations are permanently disabled inside AppData to protect application configurations and local data.");
    }
    if norm_case.starts_with(&format!("{users_case}\\")) {
        if let Some(part) = parts.iter().find(|p| p.starts_with('.') && **p != "." && **p != "..") {
            return blocked(&format!("Operations are permanently disabled on hidden application configuration folders ({part})."));
        }
    }
    // 5. Python and other runtimes
    const RUNTIME: &str = "Operations are permanently disabled on Python and runtime environment installations.";
    if parts.iter().any(|p| ["python", "pymanager", "miniconda", "anaconda", "virtualenv", "venv"].iter().any(|r| p.starts_with(r))) {
        return blocked(RUNTIME);
    }
    let mut check = vec![norm.clone()];
    let parent = dirname(&norm);
    if !parent.is_empty() && parent != norm {
        check.push(parent);
    }
    for d in &check {
        if ["python.exe", "py.exe", "pyvenv.cfg"].iter().any(|f| exists(&join(&[d, f]))) || exists(&join(&[d, "Scripts", "pip.exe"])) {
            return blocked(RUNTIME);
        }
    }
    // 6. Programs and games at the top of the system drive
    if norm_case.starts_with(&norm_sys_root) {
        let rel = relpath(&norm, &sys_root).unwrap_or_default();
        let top = rel.split('\\').next().unwrap_or("").to_lowercase();
        const KNOWN: &[&str] = &[
            "games", "xboxgames", "riot games", "steamcmd", "steam", "steamlibrary", "epic games", "gog games", "ubisoft", "battle.net", "qt",
            "certbot", "xamp", "xampp", "wamp", "nginx", "apache", "tools", "bin", "vcpkg", "mingw", "cygwin", "msys", "llvm", "prism hub",
            "rootline prism",
        ];
        if KNOWN.contains(&top.as_str()) || top.starts_with("borderless") || top.starts_with("app") || top.starts_with("game") {
            return blocked(&format!("Operations are permanently disabled on application and game installation directories ({top})."));
        }
        const SAFE: &[&str] = &["downloads", "testdownloads", "test", "tmp", "workspace", "media", "data", "backup", "kemono", "pawchive"];
        if !SAFE.contains(&top.as_str()) {
            if let Ok(rd) = fs::read_dir(join(&[&sys_root, &top])) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_lowercase();
                    let ext = splitext(&name).1.to_string();
                    if matches!(ext.as_str(), ".exe" | ".dll" | ".sys") || name.starts_with("uninstall") || name.starts_with("unins") {
                        return blocked(&format!("Operations are permanently disabled on application and software directories ({top})."));
                    }
                }
            }
        }
    }
    // 7. Other drives' roots: allowed, with double confirmation
    let other_root = is_drive_root && !is_system_root;
    let msg = if other_root {
        format!("Target is the root of drive ({drive_letter}\\). Executing operations here affects the entire drive and requires double confirmation.")
    } else {
        String::new()
    };
    answer(false, false, false, is_drive_root, other_root, &msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        signals: Mutex<Vec<(String, Vec<Value>)>>,
        dir: String,
    }

    impl Outside for Rec {
        fn signal(&self, name: &str, args: Vec<Value>) {
            self.signals.lock().unwrap().push((name.into(), args));
        }
        fn log(&self, _level: &str, _message: &str, _details: &str) {}
        fn download_dir(&self) -> String {
            self.dir.clone()
        }
    }

    fn close(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-6,
            (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| close(a, b)),
            (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| close(v, w))),
            _ => a == b,
        }
    }

    fn build(base: &str, tree: &[Value]) {
        let _ = fs::remove_dir_all(base);
        fs::create_dir_all(base).unwrap();
        for t in tree {
            let p = join(&[base, &t[0].as_str().unwrap().replace('/', "\\")]);
            match t[1].as_str() {
                None => fs::create_dir_all(&p).unwrap(),
                Some(c) => {
                    fs::create_dir_all(dirname(&p)).unwrap();
                    fs::write(&p, c).unwrap();
                }
            }
        }
    }

    /// .venv/Scripts/python tools/golden_gallery.py, then: cargo test --lib -- --ignored gallery
    #[test]
    #[ignore]
    fn gallery_matches_python_live() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/target/gallery_live.json");
        let want: Value = serde_json::from_str(&fs::read_to_string(path).expect("run tools/golden_gallery.py first")).unwrap();
        let base = want["base"].as_str().unwrap().to_string();
        let rec = Arc::new(Rec { dir: base.clone(), ..Default::default() });
        let g = Gallery::new(rec.clone());
        let mut wrong = Vec::new();
        for c in want["cases"].as_array().unwrap() {
            let a = c["args"].as_array().unwrap();
            let s = |i: usize| a[i].as_str().unwrap().to_string();
            let got = match c["fn"].as_str().unwrap() {
                "listDirectory" => {
                    let items = g.list_directory(&s(0), a[1].as_u64().unwrap() as usize);
                    json!({"items": items, "total": g.last_listing_total()})
                }
                "folderStats" => {
                    rec.signals.lock().unwrap().clear();
                    g.folder_stats(&abspath(&s(0)), g.stats_token.load(Ordering::Relaxed));
                    json!(rec.signals.lock().unwrap().last().map(|x| x.1.clone()))
                }
                "getBreadcrumbs" => json!(g.breadcrumbs(&s(0))),
                "scanDuplicates" => json!(g.scan_duplicates(&s(0), a[1].as_bool().unwrap())),
                "getPathSafetyInfo" => path_safety_info(&s(0)),
                f => panic!("{f}"),
            };
            if !close(&got, &c["result"]) {
                wrong.push(format!("{} {}\n  python: {}\n  rust:   {}", c["fn"], c["args"], c["result"], got));
            }
        }
        // Moves and renames, on a copy of the tree
        let ops = format!("{base}_rs");
        build(&ops, want["tree"].as_array().unwrap());
        let sub = |v: &Value| -> Value { serde_json::from_str(&v.to_string().replace("{OPS}", &ops.replace('\\', "\\\\"))).unwrap() };
        for st in want["ops"]["steps"].as_array().unwrap() {
            let a = sub(&st["args"]);
            let got = match st["fn"].as_str().unwrap() {
                "moveItems" => {
                    let paths: Vec<String> = a[0].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
                    g.move_items(&paths, a[1].as_str().unwrap())
                }
                "renameItem" => g.rename_item(a[0].as_str().unwrap(), a[1].as_str().unwrap()),
                f => panic!("{f}"),
            };
            if !close(&got, &sub(&st["result"])) {
                wrong.push(format!("{} {}\n  python: {}\n  rust:   {}", st["fn"], a, sub(&st["result"]), got));
            }
        }
        let mut after = Vec::new();
        let mut stack = vec![ops.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let p = e.path().to_string_lossy().into_owned();
                after.push(relpath(&p, &ops).unwrap());
                if e.file_type().unwrap().is_dir() {
                    stack.push(p);
                }
            }
        }
        after.sort();
        if json!(after) != want["ops"]["after"] {
            wrong.push(format!("files after the moves\n  python: {}\n  rust:   {}", want["ops"]["after"], json!(after)));
        }
        let _ = fs::remove_dir_all(&ops);
        assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    #[test]
    fn drive_space_labels() {
        let t = Gallery::drive_space_info("C:\\");
        assert!(t["total_bytes"].as_u64().unwrap() > 0);
        assert!(t["space_label"].as_str().unwrap().contains('/'));
        assert!(Gallery::drive_space_info("Q:\nope-not-a-drive")["space_label"] == "");
    }
}
