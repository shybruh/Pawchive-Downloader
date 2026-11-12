//! "galleryTools": the Gallery's search inside subfolders (file names, and the posts' titles, text
//! and tags read from the post_info.txt files beside the files), new folders, and undoing moves,
//! renames and new folders. A port of bridge/gallery_tools_bridge.py and
//! core/post_metadata_index.py, checked against them by tests/fixtures/gallery_tools.json
//! (tools/golden_gallery_tools.py).

use super::gallery::{move_path, Gallery};
use crate::decompress::bank::{info_dirs_for, post_info_files};
use crate::downloader::py::{basename, dirname, join, normcase, normpath, relpath, splitext};
use crate::downloader::service;
use crate::host::native::{Events, NativeObject, Prop};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, UNIX_EPOCH};

pub const NAME: &str = "galleryTools";
const UNDO_LIMIT: usize = 25;

// ── The search's words ────────────────────────────────────────────────────────────────────────

/// shlex.split (POSIX rules: quotes, backslashes); Err for an unclosed quote or a lone backslash
fn shlex_split(s: &str) -> Result<Vec<String>, ()> {
    let ws = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');
    let mut out = Vec::new();
    let mut token = String::new();
    let mut have = false; // a token was started (also an empty quoted one)
    let mut state = ' ';
    let mut esc_from = ' ';
    for c in s.chars() {
        match state {
            ' ' | 'a' => {
                if ws(c) {
                    if state == 'a' {
                        out.push(std::mem::take(&mut token));
                        have = false;
                    }
                    state = ' ';
                } else if c == '\\' {
                    esc_from = 'a';
                    state = '\\';
                    have = true;
                } else if c == '"' || c == '\'' {
                    state = c;
                    have = true;
                } else {
                    token.push(c);
                    have = true;
                    state = 'a';
                }
            }
            '"' | '\'' => {
                if c == state {
                    state = 'a';
                } else if c == '\\' && state == '"' {
                    esc_from = state;
                    state = '\\';
                } else {
                    token.push(c);
                }
            }
            _ => {
                // after a backslash
                if (esc_from == '"' || esc_from == '\'') && c != esc_from && c != '\\' {
                    token.push('\\');
                }
                token.push(c);
                state = esc_from;
            }
        }
    }
    match state {
        '"' | '\'' | '\\' => Err(()),
        _ => {
            if have || state == 'a' {
                out.push(token);
            }
            Ok(out)
        }
    }
}

fn strip_quotes(s: &str) -> &str {
    s.trim_matches(|c| c == '"' || c == '\'' || c == ' ')
}

/// A search split into name / text words, file types and tags
pub fn parse_query(query: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut words, mut exts, mut tags) = (Vec::new(), Vec::new(), Vec::new());
    let q = query.trim();
    let tokens = shlex_split(q).unwrap_or_else(|_| q.split_whitespace().map(str::to_string).collect());
    for tok in tokens {
        let clean = strip_quotes(&tok);
        let lower = clean.to_lowercase();
        if lower.starts_with("tag:") {
            let v = strip_quotes(&clean[4..]);
            if !v.is_empty() {
                tags.push(v.to_string());
            }
        } else if lower.starts_with("ext:") {
            exts.push(format!(".{}", clean[4..].trim_start_matches('.').to_lowercase()));
        } else if lower.starts_with("*.") {
            let v = &lower[1..];
            if v.chars().count() > 1 {
                exts.push(v.to_string());
            }
        } else if lower.starts_with('.') && lower.chars().count() > 1 {
            exts.push(lower);
        } else if !lower.is_empty() {
            words.push(lower);
        }
    }
    (words, exts, tags)
}

fn name_matches(name: &str, is_dir: bool, words: &[String], exts: &[String]) -> bool {
    let lower = name.to_lowercase();
    if !exts.is_empty() && (is_dir || !exts.iter().any(|e| lower.ends_with(e.as_str()))) {
        return false;
    }
    words.iter().all(|w| lower.contains(w.as_str()))
}

// ── The posts in post_info.txt ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub post_id: String,
    pub title: String,
    pub creator: String,
    pub service: String,
    pub tags: Vec<String>,
    pub content: String,
    pub attached: HashSet<String>,
    /// title, creator, service, tags and the text's start, lowercased: what words are looked for in
    pub blob: String,
}

impl Post {
    fn matches(&self, words: &[String], tags: &[String]) -> bool {
        let own: Vec<String> = self.tags.iter().map(|t| t.to_lowercase()).collect();
        tags.iter().all(|t| {
            let t = t.to_lowercase();
            own.iter().any(|o| o.contains(&t))
        }) && words.iter().all(|w| self.blob.contains(&w.to_lowercase()))
    }
}

fn py_strip(s: &str) -> &str {
    s.trim()
}

/// str.splitlines (the text already has \n line ends)
fn split_lines(s: &str) -> Vec<&str> {
    s.split(['\n', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}']).collect()
}

/// The posts written in a post_info.txt (one, or several separated by ===== lines)
pub fn parse_posts(text: &str) -> Vec<Post> {
    static SPLIT: OnceLock<Regex> = OnceLock::new();
    let split = SPLIT.get_or_init(|| Regex::new(r"(?:\r?\n)?={30,}(?:\r?\n)?").unwrap());
    if text.trim().is_empty() {
        return Vec::new();
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut posts = Vec::new();
    for block in split.split(&text) {
        let block = py_strip(block);
        if block.is_empty() {
            continue;
        }
        let mut meta: HashMap<String, String> = HashMap::new();
        let (mut content, mut attached) = (Vec::new(), HashSet::new());
        let (mut in_content, mut in_files, mut in_header) = (false, false, true);
        for line in split_lines(block) {
            let trimmed = py_strip(line);
            if trimmed.starts_with("--- ") {
                in_header = false;
                in_content = trimmed == "--- Content ---";
                in_files = trimmed == "--- Attached Files ---";
                continue;
            }
            if in_header {
                if let Some((k, v)) = line.split_once(':') {
                    meta.insert(py_strip(k).to_lowercase(), py_strip(v).to_string());
                }
            } else if in_content {
                content.push(line);
            } else if in_files && !trimmed.is_empty() {
                attached.insert(trimmed.to_string());
            }
        }
        let get = |k: &str| meta.get(k).cloned().unwrap_or_default();
        let (post_id, title, mut creator) = (get("post id"), get("title"), get("creator"));
        let mut service = String::new();
        if creator.contains('[') && creator.ends_with(']') {
            let i = creator.rfind('[').unwrap();
            service = py_strip(&creator[i + 1..creator.len() - 1]).to_string();
            creator = py_strip(&creator[..i]).to_string();
        }
        let raw_tags = get("tags");
        let tags: Vec<String> = raw_tags.split(',').map(py_strip).filter(|t| !t.is_empty()).map(str::to_string).collect();
        let content = py_strip(&content.join("\n")).to_string();
        if title.is_empty() && post_id.is_empty() && tags.is_empty() && content.is_empty() && attached.is_empty() {
            continue;
        }
        let attached: HashSet<String> = attached.iter().map(|f| py_strip(f).to_lowercase()).filter(|f| !f.is_empty()).collect();
        let (post_id, title, creator, service) =
            (py_strip(&post_id).to_string(), py_strip(&title).to_string(), py_strip(&creator).to_string(), py_strip(&service).to_string());
        let mut parts: Vec<String> = vec![title.clone(), creator.clone(), service.clone()];
        parts.extend(tags.iter().cloned());
        if !content.is_empty() {
            parts.push(content.chars().take(4000).collect());
        }
        let blob = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ").to_lowercase();
        posts.push(Post { post_id, title, creator, service, tags, content, attached, blob });
    }
    posts
}

fn mtime_of(m: &fs::Metadata) -> f64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The posts read so far (a file is read again when it changes), and each folder's post texts
#[derive(Default)]
struct PostCache {
    files: HashMap<String, (f64, u64, Arc<Vec<Post>>)>,
    folders: HashMap<String, (f64, Vec<String>)>,
}

const CACHE_MAX: usize = 25000;

fn cache() -> &'static Mutex<PostCache> {
    static C: OnceLock<Mutex<PostCache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn file_posts(info: &str) -> Arc<Vec<Post>> {
    let Ok(m) = fs::metadata(info) else { return Arc::new(Vec::new()) };
    let (mt, size, key) = (mtime_of(&m), m.len(), normcase(&normpath(info)));
    if let Some((t, s, p)) = cache().lock().unwrap().files.get(&key) {
        if *t == mt && *s == size {
            return p.clone();
        }
    }
    let posts = Arc::new(fs::read(info).map(|b| parse_posts(&String::from_utf8_lossy(&b))).unwrap_or_default());
    let mut c = cache().lock().unwrap();
    if c.files.len() >= CACHE_MAX {
        c.files.clear();
    }
    c.files.insert(key, (mt, size, posts.clone()));
    posts
}

/// The posts belonging to a folder's files: its post texts, and the "Other" folder's when files
/// are grouped by type
fn folder_posts(folder: &str) -> Vec<Post> {
    let Ok(m) = fs::metadata(folder) else { return Vec::new() };
    if folder.is_empty() || !m.is_dir() {
        return Vec::new();
    }
    let (mt, key) = (mtime_of(&m), normcase(&normpath(folder)));
    let known = cache().lock().unwrap().folders.get(&key).filter(|(t, _)| *t == mt).map(|(_, f)| f.clone());
    let infos = known.unwrap_or_else(|| {
        let mut infos = Vec::new();
        for d in info_dirs_for(folder) {
            if fs::metadata(&d).map(|m| m.is_dir()).unwrap_or(false) {
                infos.extend(post_info_files(&d, 20));
            }
        }
        let mut c = cache().lock().unwrap();
        if c.folders.len() >= CACHE_MAX {
            c.folders.clear();
        }
        c.folders.insert(key, (mt, infos.clone()));
        infos
    });
    infos.iter().flat_map(|i| file_posts(i).as_ref().clone()).collect()
}

/// (every file of the folder matches, the files named by the posts that match)
fn search_folder(folder: &str, words: &[String], tags: &[String]) -> (bool, HashSet<String>) {
    let posts = folder_posts(folder);
    if posts.is_empty() {
        return (false, HashSet::new());
    }
    let (mut files, mut any) = (HashSet::new(), false);
    for p in &posts {
        if p.matches(words, tags) {
            any = true;
            files.extend(p.attached.iter().cloned());
        }
    }
    (any && (posts.len() == 1 || files.is_empty()), files)
}

// ── The search ────────────────────────────────────────────────────────────────────────────────

/// A found file or folder, as the Gallery's listing has it, with its folder under the search root
fn found_item(path: &str, name: &str, is_dir: bool, root: &str) -> Option<Value> {
    let m = fs::symlink_metadata(path).ok()?;
    let rel = relpath(&dirname(path), root).unwrap_or_default();
    Some(json!({
        "name": name, "path": normpath(path), "is_dir": is_dir,
        "size": if is_dir { -1 } else { m.len() as i64 },
        "file_count": if is_dir { -1 } else { 0 }, "folder_count": 0, "child_count": if is_dir { -1 } else { 0 },
        "mtime": mtime_of(&m),
        "ext": if is_dir { String::new() } else { splitext(name).1.to_lowercase() },
        "rel_dir": if rel == "." { String::new() } else { rel },
    }))
}

/// Looks through every folder under root; `emit(items, done, folders looked in)` gets the finds in
/// batches. Stops when `stale()` turns true.
pub fn search(root: &str, words: &[String], exts: &[String], tags: &[String], max: usize, stale: &dyn Fn() -> bool,
              emit: &mut dyn FnMut(Vec<Value>, bool, u64)) {
    let (mut batch, mut found, mut scanned) = (Vec::new(), 0usize, 0u64);
    let mut last = Instant::now();
    let mut stack = vec![root.to_string()];
    while let Some(folder) = stack.pop() {
        if stale() {
            return;
        }
        scanned += 1;
        let Ok(rd) = fs::read_dir(&folder) else { continue };
        let entries: Vec<fs::DirEntry> = rd.flatten().collect();
        let (all, files) = if !words.is_empty() || !tags.is_empty() { search_folder(&folder, words, tags) } else { (false, HashSet::new()) };
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = join(&[&folder, &name]);
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                stack.push(path.clone());
            }
            let mut hit = name_matches(&name, is_dir, words, exts) && tags.is_empty();
            if !hit && !is_dir {
                let lower = name.to_lowercase();
                if exts.is_empty() || exts.iter().any(|x| lower.ends_with(x.as_str())) {
                    hit = all || files.contains(&lower);
                }
            }
            if hit {
                if let Some(item) = found_item(&path, &name, is_dir, root) {
                    batch.push(item);
                    found += 1;
                    if found >= max {
                        stack.clear();
                        break;
                    }
                }
            }
        }
        if !batch.is_empty() && (batch.len() >= 300 || last.elapsed().as_secs_f64() > 0.3) {
            if stale() {
                return;
            }
            emit(std::mem::take(&mut batch), false, scanned);
            last = Instant::now();
        }
    }
    if !stale() {
        emit(batch, true, scanned);
    }
}

// ── The object ────────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
enum Undo {
    Move { pairs: Vec<(String, String)>, label: String },
    Mkdir { path: String, label: String },
}

impl Undo {
    fn label(&self) -> &str {
        match self {
            Undo::Move { label, .. } | Undo::Mkdir { label, .. } => label,
        }
    }
}

pub struct GalleryTools {
    gallery: Arc<Gallery>,
    events: Option<Events>,
    search_token: Arc<AtomicU64>,
    undo: Mutex<Vec<Undo>>,
}

fn log(level: &str, msg: &str) {
    service::log_app(level, msg, "gallery", "");
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

impl GalleryTools {
    pub fn new(gallery: Arc<Gallery>, events: Option<Events>) -> Arc<Self> {
        Arc::new(Self { gallery, events, search_token: Arc::new(AtomicU64::new(0)), undo: Mutex::new(Vec::new()) })
    }

    fn signal(&self, name: &str, args: Vec<Value>) {
        if let Some(e) = &self.events {
            e.signal(NAME, name, args);
        }
    }

    fn undo_changed(&self) {
        if let Some(e) = &self.events {
            e.props(NAME, json!({"canUndo": self.can_undo(), "undoLabel": self.undo_label()}));
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.lock().unwrap().is_empty()
    }

    pub fn undo_label(&self) -> String {
        self.undo.lock().unwrap().last().map(|u| u.label().to_string()).unwrap_or_default()
    }

    fn push(&self, op: Undo) {
        {
            let mut u = self.undo.lock().unwrap();
            u.push(op);
            let extra = u.len().saturating_sub(UNDO_LIMIT);
            u.drain(..extra);
        }
        self.undo_changed();
    }

    pub fn start_search(self: &Arc<Self>, root: &str, query: &str, max: usize) -> u64 {
        let token = self.search_token.fetch_add(1, Ordering::SeqCst) + 1;
        let (words, exts, tags) = parse_query(query);
        let is_dir = fs::metadata(root).map(|m| m.is_dir()).unwrap_or(false);
        if root.is_empty() || !is_dir || (words.is_empty() && exts.is_empty() && tags.is_empty()) {
            self.signal("searchBatch", vec![json!(token), json!([]), json!(true), json!(0)]);
            return token;
        }
        let (me, root) = (self.clone(), normpath(root));
        std::thread::Builder::new().name("gallery-search".into()).spawn(move || {
            let current = me.search_token.clone();
            let stale = move || current.load(Ordering::SeqCst) != token;
            search(&root, &words, &exts, &tags, max.max(1), &stale, &mut |items, done, scanned| {
                me.signal("searchBatch", vec![json!(token), json!(items), json!(done), json!(scanned)]);
            });
        }).ok();
        token
    }

    pub fn cancel_search(&self) {
        self.search_token.fetch_add(1, Ordering::SeqCst);
    }

    pub fn move_items(&self, paths: &[String], dest: &str) -> Value {
        let res = self.gallery.move_items(paths, dest);
        let pairs: Vec<(String, String)> = res["pairs"].as_array().into_iter().flatten()
            .map(|p| (p[0].as_str().unwrap_or("").to_string(), p[1].as_str().unwrap_or("").to_string())).collect();
        if !pairs.is_empty() {
            let n = pairs.len();
            self.push(Undo::Move { pairs, label: format!("Undo move of {n} item{}", plural(n)) });
        }
        res
    }

    pub fn rename_item(&self, path: &str, name: &str) -> Value {
        let res = self.gallery.rename_item(path, name);
        let new = res["new_path"].as_str().unwrap_or("").to_string();
        if res["success"].as_bool() == Some(true) && !new.is_empty() && normpath(&new) != normpath(path) {
            self.push(Undo::Move { pairs: vec![(normpath(path), new)], label: format!("Undo rename of '{}'", basename(path)) });
        }
        res
    }

    pub fn create_folder(&self, parent: &str, name: &str) -> Value {
        let fail = |e: &str| json!({"success": false, "error": e, "path": ""});
        let name = name.trim();
        if parent.is_empty() || !fs::metadata(parent).map(|m| m.is_dir()).unwrap_or(false) {
            return fail("The current folder no longer exists.");
        }
        if name.is_empty() || name == "." || name == ".." || name.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*']) || name.ends_with(['.', ' ']) {
            return fail("That name contains characters that aren't allowed.");
        }
        if self.gallery.path_safety(parent)["is_blocked"].as_bool() == Some(true) {
            return fail("Creating folders inside a protected system folder is blocked.");
        }
        let target = join(&[parent, name]);
        if fs::symlink_metadata(&target).is_ok() {
            return fail(&format!("An item named '{name}' already exists here."));
        }
        if let Err(e) = fs::create_dir_all(&target) {
            return fail(&e.to_string());
        }
        self.push(Undo::Mkdir { path: target.clone(), label: format!("Undo new folder '{name}'") });
        json!({"success": true, "error": "", "path": target})
    }

    /// Takes the last operation off the list, for undoing
    fn pop(&self) -> Option<Undo> {
        let op = self.undo.lock().unwrap().pop();
        if op.is_some() {
            self.undo_changed();
        }
        op
    }

    pub fn undo_last(&self) -> Value {
        match self.pop() {
            None => json!({"success": false, "message": "Nothing to undo."}),
            Some(op) => undo(&op),
        }
    }

    pub fn undo_last_async(self: &Arc<Self>) {
        let Some(op) = self.pop() else {
            self.signal("undoFinished", vec![json!({"success": false, "message": "Nothing to undo."})]);
            return;
        };
        let me = self.clone();
        std::thread::Builder::new().name("gallery-undo".into()).spawn(move || {
            let res = undo(&op);
            me.signal("undoFinished", vec![res]);
        }).ok();
    }
}

/// Undoes one operation: moves each item back (newest first), or removes the new folder if empty
fn undo(op: &Undo) -> Value {
    let (mut errors, mut done) = (Vec::<String>::new(), 0);
    let exists = |p: &str| fs::symlink_metadata(p).is_ok();
    match op {
        Undo::Move { pairs, .. } => {
            for (src, dst) in pairs.iter().rev() {
                if !exists(dst) {
                    errors.push(format!("'{}' is no longer there", basename(dst)));
                    continue;
                }
                if exists(src) && normcase(src) != normcase(dst) {
                    errors.push(format!("'{}' already exists in its original place", basename(src)));
                    continue;
                }
                let r = fs::create_dir_all(dirname(src)).and_then(|_| move_path(dst, src));
                match r {
                    Ok(()) => done += 1,
                    Err(e) => errors.push(format!("{}: {e}", basename(dst))),
                }
            }
        }
        Undo::Mkdir { path, .. } => match fs::remove_dir(path) {
            Ok(()) => done = 1,
            Err(_) => errors.push(format!("'{}' isn't empty any more, so it was left in place", basename(path))),
        },
    }
    let undid = op.label().replacen("Undo", "Undid", 1);
    if !errors.is_empty() {
        for e in errors.iter().take(5) {
            log("warning", &format!("Undo: {e}"));
        }
        let msg = if done > 0 { format!("Partly undone ({done} done): {}", errors[0]) } else { format!("Couldn't undo: {}", errors[0]) };
        return json!({"success": done > 0, "message": msg});
    }
    log("info", &format!("{undid}."));
    json!({"success": true, "message": undid})
}

fn s(args: &[Value], i: usize) -> String {
    args.get(i).and_then(Value::as_str).unwrap_or("").to_string()
}

/// The object needs `Arc<Self>` for its background work; the host holds it in this wrapper
pub struct Object(pub Arc<GalleryTools>);

impl NativeObject for Object {
    fn props(&self) -> Vec<Prop> {
        vec![Prop { name: "canUndo", kind: "bool", writable: false }, Prop { name: "undoLabel", kind: "QString", writable: false }]
    }
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            ("startSearch", vec!["root", "query", "maxResults"]), ("cancelSearch", vec![]),
            ("moveItems", vec!["paths", "destDir"]), ("renameItem", vec!["path", "newName"]),
            ("createFolder", vec!["parent", "name"]), ("undoLastAsync", vec![]), ("undoLast", vec![]),
        ]
    }
    fn signals(&self) -> Vec<&'static str> {
        vec!["searchBatch", "undoChanged", "undoFinished"]
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        match prop {
            "canUndo" => Ok(json!(self.0.can_undo())),
            "undoLabel" => Ok(json!(self.0.undo_label())),
            _ => Err(format!("unknown property {prop:?}")),
        }
    }
    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        let t = &self.0;
        Ok(match method {
            "startSearch" => json!(t.start_search(&s(&args, 0), &s(&args, 1), args.get(2).and_then(Value::as_u64).unwrap_or(5000) as usize)),
            "cancelSearch" => {
                t.cancel_search();
                Value::Null
            }
            "moveItems" => {
                let paths: Vec<String> = args.first().and_then(Value::as_array).into_iter().flatten()
                    .filter_map(|v| v.as_str().map(str::to_string)).collect();
                t.move_items(&paths, &s(&args, 1))
            }
            "renameItem" => t.rename_item(&s(&args, 0), &s(&args, 1)),
            "createFolder" => t.create_folder(&s(&args, 0), &s(&args, 1)),
            "undoLastAsync" => {
                t.undo_last_async();
                Value::Null
            }
            "undoLast" => t.undo_last(),
            m => return Err(format!("no method {m:?}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::gallery::Outside;
    use super::*;

    struct Quiet;
    impl Outside for Quiet {
        fn signal(&self, _: &str, _: Vec<Value>) {}
        fn log(&self, _: &str, _: &str, _: &str) {}
        fn download_dir(&self) -> String {
            String::new()
        }
    }

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/gallery_tools.json")).unwrap()
    }

    fn tmp(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("pawchive_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        normpath(&fs::canonicalize(&d).unwrap().to_string_lossy().replace(r"\\?\", ""))
    }

    fn build(base: &str, tree: &serde_json::Map<String, Value>) {
        for (p, c) in tree {
            let full = join(&[base, &p.replace('/', "\\")]);
            match c.as_str() {
                None => fs::create_dir_all(&full).unwrap(),
                Some(t) => {
                    fs::create_dir_all(dirname(&full)).unwrap();
                    fs::write(&full, t).unwrap();
                }
            }
        }
    }

    fn listing(base: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![base.to_string()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let p = e.path().to_string_lossy().into_owned();
                out.push(relpath(&p, base).unwrap().replace('\\', "/"));
                if e.file_type().unwrap().is_dir() {
                    stack.push(p);
                }
            }
        }
        out.sort();
        out
    }

    fn rel(v: &Value, base: &str) -> Value {
        serde_json::from_str(&v.to_string().replace(&base.replace('\\', "\\\\"), "<B>")).unwrap()
    }

    #[test]
    fn queries_and_posts_like_python() {
        let fx = fixture();
        for q in fx["queries"].as_array().unwrap() {
            let (w, e, t) = parse_query(q[0].as_str().unwrap());
            assert_eq!(json!([w, e, t]), q[1], "query {}", q[0]);
        }
        for p in fx["posts"].as_array().unwrap() {
            let got: Vec<Value> = parse_posts(p[0].as_str().unwrap()).iter().map(|r| {
                let mut a: Vec<&String> = r.attached.iter().collect();
                a.sort();
                json!({"post_id": r.post_id, "title": r.title, "creator": r.creator, "service": r.service, "tags": r.tags,
                       "content": r.content, "attached": a, "blob": r.blob})
            }).collect();
            assert_eq!(json!(got), p[1], "post text {:?}", p[0]);
        }
    }

    #[test]
    fn searches_like_python() {
        let fx = fixture();
        let base = tmp("gsearch");
        build(&base, fx["search"]["tree"].as_object().unwrap());
        for run in fx["search"]["runs"].as_array().unwrap() {
            let (w, e, t) = parse_query(run["query"].as_str().unwrap());
            let max = run["max"].as_u64().unwrap_or(5000) as usize;
            let (mut items, mut scanned, mut done) = (Vec::new(), 0, false);
            search(&base, &w, &e, &t, max, &|| false, &mut |b, d, s| {
                items.extend(b);
                scanned = s;
                done = d;
            });
            assert!(done);
            for i in items.iter_mut() {
                assert!(i["mtime"].as_f64().unwrap() > 0.0);
                i["mtime"] = Value::Null;
            }
            assert_eq!(rel(&json!(items), &base), run["items"], "search {}", run["query"]);
            assert_eq!(json!(scanned), run["scanned"], "folders looked in for {}", run["query"]);
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn folders_and_undo_like_python() {
        let fx = fixture();
        let base = tmp("gundo");
        build(&base, fx["search"]["tree"].as_object().unwrap());
        let t = GalleryTools::new(Gallery::new(Arc::new(Quiet)), None);
        let at = |p: &Value| if p.as_str().unwrap().is_empty() { String::new() } else { join(&[&base, &p.as_str().unwrap().replace('/', "\\")]) };
        for (i, st) in fx["ops"]["steps"].as_array().unwrap().iter().enumerate() {
            let a = st["args"].as_array().unwrap();
            let op = st["op"].as_str().unwrap();
            let got = match op {
                "createFolder" => t.create_folder(&at(&a[0]), a[1].as_str().unwrap()),
                "moveItems" => t.move_items(&a[0].as_array().unwrap().iter().map(&at).collect::<Vec<_>>(), &at(&a[1])),
                "renameItem" => t.rename_item(&at(&a[0]), a[1].as_str().unwrap()),
                "undo" => t.undo_last(),
                "count" => {
                    for n in 0..27 {
                        t.create_folder(&join(&[&base, "Other"]), &format!("f{n}"));
                    }
                    json!(t.undo.lock().unwrap().len())
                }
                o => panic!("{o}"),
            };
            assert_eq!(rel(&got, &base), st["result"], "step {i} {op} {a:?}");
            assert_eq!(json!(t.can_undo()), st["canUndo"], "step {i}");
            assert_eq!(json!(t.undo_label()), st["undoLabel"], "step {i}");
            if op != "count" {
                assert_eq!(json!(listing(&base)), st["files"], "files after step {i}");
            }
            // (the Python steps wrote these files between steps)
            match (i, op) {
                (13, _) => fs::write(join(&[&base, "Other", "Holder", "keep.txt"]), "k").unwrap(),
                (17, _) => fs::write(join(&[&base, "Artist", "dog.jpg"]), "again").unwrap(),
                (19, _) => fs::remove_file(join(&[&base, "Other", "cat_one.png"])).unwrap(),
                _ => {}
            }
        }
        let _ = fs::remove_dir_all(&base);
    }
}
