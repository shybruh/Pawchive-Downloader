//! The Decompressor's password bank: passwords by creator ("Global" for the rest) in
//! config/decompressor_passwords.json, the password that opened each archive in
//! archive_password_map.json, and the passwords to try on an archive (its own, the post texts
//! beside it, the Link Vault's, the bank's, the creator's name). A port of
//! core/archive_password_manager.py, with the same files; checked against it by
//! tests/fixtures/decompress.json.

use crate::downloader::extract;
use crate::downloader::py::{abspath, basename, dirname, normcase, normpath, splitext};
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

/// What the bank reads from the Link Vault: its creators and posts, and every password in it
#[derive(Default)]
pub struct VaultView {
    pub creators: Map<String, Value>,
    pub posts: Map<String, Value>,
    pub all_passwords: Vec<String>,
}

pub struct Bank {
    dir: String,
    creators: IndexMap<String, Vec<String>>,
    flat: Vec<String>,
    archives: Map<String, Value>,
    log: Box<dyn Fn(&str, &str) + Send>,
}

fn text(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        other => crate::downloader::py::pystr(other),
    }
}

impl Bank {
    fn passwords_file(&self) -> String {
        Path::new(&self.dir).join("decompressor_passwords.json").to_string_lossy().into_owned()
    }
    fn map_file(&self) -> String {
        Path::new(&self.dir).join("archive_password_map.json").to_string_lossy().into_owned()
    }

    pub fn load(dir: &str, log: Box<dyn Fn(&str, &str) + Send>) -> Self {
        let mut b = Bank { dir: dir.into(), creators: IndexMap::new(), flat: Vec::new(), archives: Map::new(), log };
        let clean = |v: &Value| -> Vec<String> {
            v.as_array().into_iter().flatten().map(|p| text(p).trim().to_string()).filter(|p| !p.is_empty()).collect()
        };
        if let Ok(s) = std::fs::read_to_string(b.passwords_file()) {
            match serde_json::from_str::<Value>(&s) {
                Ok(Value::Array(_)) => {
                    let pws = clean(&serde_json::from_str::<Value>(&s).unwrap());
                    if !pws.is_empty() {
                        b.creators.insert("Global".into(), pws);
                    }
                }
                Ok(Value::Object(d)) => {
                    for (c, pws) in d.get("creators").and_then(Value::as_object).into_iter().flatten() {
                        if pws.is_array() {
                            let c2 = clean(pws);
                            if !c2.is_empty() {
                                b.creators.insert(c.clone(), c2);
                            }
                        }
                    }
                    let g = clean(d.get("global").unwrap_or(&Value::Null));
                    if !g.is_empty() {
                        b.creators.insert("Global".into(), g);
                    }
                }
                Ok(_) => {}
                Err(e) => (b.log)("warning", &format!("Could not load decompressor_passwords.json: {e}")),
            }
        }
        b.rebuild();
        if let Ok(s) = std::fs::read_to_string(b.map_file()) {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&s) {
                b.archives = m;
            }
        }
        b
    }

    fn rebuild(&mut self) {
        let mut seen = HashSet::new();
        let mut all = Vec::new();
        for (c, pws) in &self.creators {
            if c != "Global" {
                for p in pws {
                    if seen.insert(p.clone()) {
                        all.push(p.clone());
                    }
                }
            }
        }
        for p in self.creators.get("Global").into_iter().flatten() {
            if seen.insert(p.clone()) {
                all.push(p.clone());
            }
        }
        self.flat = all;
    }

    fn write(path: &str, v: &Value) -> std::io::Result<()> {
        if let Some(d) = Path::new(path).parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default())?;
        std::fs::rename(&tmp, path)
    }

    fn save_passwords(&self) {
        let creators: Map<String, Value> = self.creators.iter().filter(|(k, v)| *k != "Global" && !v.is_empty()).map(|(k, v)| (k.clone(), json!(v))).collect();
        let v = json!({"version": 2, "global": self.creators.get("Global").cloned().unwrap_or_default(), "creators": creators});
        if let Err(e) = Self::write(&self.passwords_file(), &v) {
            (self.log)("error", &format!("Failed to save decompressor_passwords.json: {e}"));
        }
    }

    fn save_map(&self) {
        let _ = Self::write(&self.map_file(), &Value::Object(self.archives.clone()));
    }

    pub fn passwords(&self) -> Vec<String> {
        self.flat.clone()
    }

    pub fn by_creator(&self) -> Value {
        Value::Object(self.creators.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), json!(v))).collect())
    }

    fn group(creator: &str) -> String {
        let c = creator.trim();
        if c.is_empty() { "Global".into() } else { c.into() }
    }

    pub fn add(&mut self, password: &str, creator: &str) -> bool {
        let p = password.trim().to_string();
        let c = Self::group(creator);
        if p.is_empty() {
            return false;
        }
        let list = self.creators.entry(c.clone()).or_default();
        if list.contains(&p) {
            return false;
        }
        list.push(p.clone());
        self.rebuild();
        self.save_passwords();
        (self.log)("info", &format!("Added a password to the Decompressor Password Bank [{c}] ({} characters).", p.chars().count()));
        true
    }

    pub fn add_many(&mut self, passwords: &[Value], creator: &str) -> usize {
        let c = Self::group(creator);
        let list = self.creators.entry(c.clone()).or_default();
        let mut added = 0;
        for item in passwords {
            let p = if item.is_null() { String::new() } else { text(item) }.trim().to_string();
            if !p.is_empty() && !list.contains(&p) {
                list.push(p);
                added += 1;
            }
        }
        if added > 0 {
            self.rebuild();
            self.save_passwords();
            (self.log)("info", &format!("Added {added} password(s) to Decompressor Password Bank [{c}]."));
        }
        added
    }

    pub fn remove(&mut self, password: &str, creator: &str) -> bool {
        let p = password.trim().to_string();
        let c = creator.trim().to_string();
        let mut removed = false;
        let groups: Vec<String> = if c.is_empty() { self.creators.keys().cloned().collect() } else { vec![c] };
        for g in groups {
            if let Some(list) = self.creators.get_mut(&g) {
                if let Some(i) = list.iter().position(|x| *x == p) {
                    list.remove(i);
                    if list.is_empty() && g != "Global" {
                        self.creators.shift_remove(&g);
                    }
                    removed = true;
                }
            }
        }
        let before = self.archives.len();
        self.archives.retain(|_, v| v.as_str() != Some(p.as_str()));
        removed |= self.archives.len() != before;
        if removed {
            self.rebuild();
            self.save_passwords();
            self.save_map();
            (self.log)("info", "Removed a password from the Decompressor Password Bank.");
        }
        removed
    }

    pub fn clear(&mut self) -> bool {
        self.flat.clear();
        self.creators.clear();
        self.archives.clear();
        self.save_passwords();
        self.save_map();
        (self.log)("info", "Cleared Decompressor Password Bank.");
        true
    }

    /// The password that opened `archive` (kept with it, and in the bank under the creator)
    pub fn record(&mut self, archive: &str, password: &str, creator: &str) {
        if archive.is_empty() || password.is_empty() {
            return;
        }
        let p = password.trim().to_string();
        self.archives.insert(normcase(&abspath(archive)), json!(p));
        self.save_map();
        self.add(&p, creator);
    }

    pub fn for_archive(&self, archive: &str) -> Option<String> {
        if archive.is_empty() {
            return None;
        }
        self.archives.get(&normcase(&abspath(archive))).and_then(Value::as_str).map(str::to_string)
    }

    /// Every password in the Link Vault into the bank: its posts' under their creators, the rest
    /// as Global
    pub fn sync_from_vault(&mut self, v: &VaultView) -> usize {
        let mut added = 0;
        for post in v.posts.values() {
            let name = post.get("creator_name").filter(|x| crate::downloader::filters::truthy(x)).map(text).unwrap_or_default();
            let name = { let n = name.trim(); if n.is_empty() { "Global".to_string() } else { n.to_string() } };
            for p in post.get("passwords").and_then(Value::as_array).into_iter().flatten() {
                let val = if crate::downloader::filters::truthy(p) { text(p) } else { String::new() }.trim().to_string();
                if val.is_empty() {
                    continue;
                }
                let list = self.creators.entry(name.clone()).or_default();
                if !list.contains(&val) {
                    list.push(val);
                    added += 1;
                }
            }
        }
        for p in &v.all_passwords {
            let val = p.trim().to_string();
            if !val.is_empty() && !self.flat.contains(&val) {
                let list = self.creators.entry("Global".into()).or_default();
                if !list.contains(&val) {
                    list.push(val);
                    added += 1;
                }
            }
        }
        if added > 0 {
            self.rebuild();
            self.save_passwords();
            (self.log)("info", &format!("Synced {added} password(s) from Link Vault into creator Password Bank."));
        }
        added
    }

    /// The passwords to try on an archive, in order: the one that opened it before, the post texts
    /// beside it, the creator's in the Link Vault and in the bank, the whole bank, the Link Vault's,
    /// then the creator's name
    pub fn candidates(&self, archive: &str, creator: &str, v: &VaultView) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |c: &str| {
            let c = c.trim();
            if !c.is_empty() && !out.iter().any(|x| x == c) {
                out.push(c.to_string());
            }
        };
        if let Some(p) = self.for_archive(archive) {
            add(&p);
        }
        if !archive.is_empty() {
            let dir = dirname(&abspath(archive));
            let name = basename(archive).to_string();
            let mut dirs = info_dirs_for(&dir);
            let parent = dirname(&dir);
            if !parent.is_empty() && parent != dir {
                dirs.push(parent);
            }
            for d in dirs {
                for p in folder_passwords(&d, &name) {
                    add(&p);
                }
            }
        }
        if !creator.is_empty() {
            let c = creator.trim().to_lowercase();
            for (key, data) in &v.creators {
                let name = data.get("name").map(text).unwrap_or_default();
                if key.to_lowercase().contains(&c) || name.to_lowercase().contains(&c) {
                    for post in v.posts.values() {
                        let same = post.get("creator_id").and_then(Value::as_str) == Some(key.as_str())
                            || post.get("creator_name").map(text).unwrap_or_default().to_lowercase() == c;
                        if same {
                            for p in post.get("passwords").and_then(Value::as_array).into_iter().flatten() {
                                if crate::downloader::filters::truthy(p) {
                                    add(&text(p));
                                }
                            }
                        }
                    }
                }
            }
            for (name, pws) in &self.creators {
                if name.trim().to_lowercase() == c {
                    for p in pws {
                        add(p);
                    }
                }
            }
        }
        for p in &self.flat {
            add(p);
        }
        for p in &v.all_passwords {
            add(p);
        }
        if !creator.is_empty() {
            let c = creator.trim();
            add(c);
            add(&c.to_lowercase());
        }
        out
    }
}

// ── Post texts beside an archive (core/path_utils.py) ─────────────────────────────────────────

const TYPE_FOLDERS: &[&str] = &["images", "video", "archive", "audio", "other"];

pub fn is_post_info_name(name: &str) -> bool {
    re!(r"(?i)^post_info(?: \[[^\]/\\]+\])?\.txt$").is_match(name)
}

/// The folders that can hold a file's post text: its own, and the "Other" one when files are
/// grouped by type (Post/Images → Post/Other, Creator/Images/Post → Creator/Other/Post)
pub fn info_dirs_for(folder: &str) -> Vec<String> {
    let mut dirs = vec![folder.to_string()];
    let parts: Vec<String> = normpath(folder).split('\\').map(str::to_string).collect();
    let n = parts.len() as isize;
    let low = (n - 3).max(0);
    let mut i = n - 1;
    while i >= low {
        let p = parts[i as usize].to_lowercase();
        if TYPE_FOLDERS.contains(&p.as_str()) {
            if p != "other" {
                let mut q = parts.clone();
                q[i as usize] = "Other".into();
                dirs.push(q.join("\\"));
            }
            break;
        }
        i -= 1;
    }
    dirs
}

pub fn post_info_files(folder: &str, limit: usize) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| is_post_info_name(n)).collect();
    names.sort_by_key(|n| (n.to_lowercase() != "post_info.txt", n.to_lowercase()));
    names.into_iter().take(limit).map(|n| Path::new(folder).join(n).to_string_lossy().into_owned()).collect()
}

fn attached_files(info: &str) -> Vec<String> {
    let Ok(b) = std::fs::read(info) else { return Vec::new() };
    let s = String::from_utf8_lossy(&b);
    let mut in_files = false;
    let mut out = Vec::new();
    for line in s.split_inclusive('\n') {
        if line.starts_with("--- ") {
            in_files = line.trim() == "--- Attached Files ---";
            continue;
        }
        if in_files && !line.trim().is_empty() {
            out.push(line.trim().to_string());
        }
    }
    out
}

/// The post text in `folder` belonging to `file_name`: "post_info.txt", else the per-post one
/// that lists the file
fn post_info_for_file(folder: &str, file_name: &str) -> String {
    let plain = Path::new(folder).join("post_info.txt");
    if plain.is_file() {
        return plain.to_string_lossy().into_owned();
    }
    if file_name.is_empty() {
        return String::new();
    }
    let (stem, ext) = splitext(file_name);
    let base = format!("{}{ext}", re!(r"(?: \[[^\]]+\])?(?: \(\d+\))?$").replace(stem, ""));
    let lists = |info: &str, f: &str| attached_files(info).iter().any(|n| n.to_lowercase() == f.trim().to_lowercase());
    for info in post_info_files(folder, 500) {
        if lists(&info, file_name) || (base != file_name && lists(&info, &base)) {
            return info;
        }
    }
    String::new()
}

/// The passwords written in a folder's text files (info.txt, post_info.txt, the archive's own post
/// text first; three files at most)
pub fn folder_passwords(folder: &str, file_name: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    if folder.is_empty() || !Path::new(folder).exists() {
        return found;
    }
    let candidates = ["info.txt", "post_info.txt"];
    let mut txt: Vec<String> = match std::fs::read_dir(folder) {
        Ok(rd) => rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|f| f.to_lowercase().ends_with(".txt") && (f.to_lowercase() == "post_info.txt" || !is_post_info_name(f))).collect(),
        Err(_) => candidates.iter().map(|s| s.to_string()).collect(),
    };
    for c in candidates {
        if let Some(i) = txt.iter().position(|t| t == c) {
            let t = txt.remove(i);
            txt.insert(0, t);
        }
    }
    let own = if file_name.is_empty() { String::new() } else { post_info_for_file(folder, file_name) };
    if !own.is_empty() {
        let b = basename(&own).to_string();
        if !txt.contains(&b) {
            txt.insert(0, b);
        }
    }
    for name in txt.iter().take(3) {
        let p = Path::new(folder).join(name);
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let content: String = String::from_utf8_lossy(&bytes).chars().take(32768).collect();
        if let Some((_, after)) = content.split_once("--- Detected Password(s) ---") {
            let section = after.split_once("\n---").map(|x| x.0).unwrap_or(after);
            for line in section.lines() {
                let pw = line.trim();
                if !pw.is_empty() && !found.iter().any(|f| f == pw) {
                    found.push(pw.to_string());
                }
            }
        }
        for pw in extract::passwords(&content) {
            if !pw.is_empty() && !found.contains(&pw) {
                found.push(pw);
            }
        }
    }
    found
}

// ── The banks, by settings folder ─────────────────────────────────────────────────────────────

fn banks() -> &'static Mutex<HashMap<String, Bank>> {
    static B: OnceLock<Mutex<HashMap<String, Bank>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

fn vault_view(path: &str) -> VaultView {
    match crate::vault::view(path) {
        Some((creators, posts, all)) => VaultView { creators, posts, all_passwords: all },
        None => VaultView::default(),
    }
}

/// Call "decompress" op "bank": {"dir", "do", …, "vault"} → the answer
pub fn call(a: &Value) -> Result<Value, String> {
    let dir = a["dir"].as_str().ok_or("no dir")?.to_string();
    let mut all = banks().lock().unwrap();
    let b = all.entry(dir.clone()).or_insert_with(|| Bank::load(&dir, Box::new(|l: &str, m: &str| crate::downloader::service::log_app(l, m, "decompressor", ""))));
    let st = |k: &str| a[k].as_str().unwrap_or("").to_string();
    Ok(match a["do"].as_str().unwrap_or("") {
        "reload" => {
            let fresh = Bank::load(&dir, Box::new(|l: &str, m: &str| crate::downloader::service::log_app(l, m, "decompressor", "")));
            *b = fresh;
            Value::Null
        }
        "passwords" => json!(b.passwords()),
        "by_creator" => b.by_creator(),
        "add" => json!(b.add(&st("password"), &st("creator"))),
        "add_many" => json!(b.add_many(a["passwords"].as_array().map(Vec::as_slice).unwrap_or(&[]), &st("creator"))),
        "remove" => json!(b.remove(&st("password"), &st("creator"))),
        "clear" => json!(b.clear()),
        "record" => {
            b.record(&st("archive"), &st("password"), &st("creator"));
            Value::Null
        }
        "for_archive" => json!(b.for_archive(&st("archive"))),
        "sync" => json!(b.sync_from_vault(&vault_view(&st("vault")))),
        "candidates" => json!(b.candidates(&st("archive"), &st("creator"), &vault_view(&st("vault")))),
        other => return Err(format!("unknown bank op {other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_bank() {
        let fx: Value = serde_json::from_str(include_str!("../../tests/fixtures/decompress.json")).unwrap();
        let bk = &fx["bank"];
        let d = std::env::temp_dir().join(format!("pawchive_bank_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let base = normpath(&std::fs::canonicalize(&d).unwrap().to_string_lossy().replace(r"\\?\", ""));
        for (name, t) in bk["info_files"].as_object().unwrap() {
            let p = Path::new(&base).join(name.replace('/', "\\"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, t.as_str().unwrap()).unwrap();
        }
        let vault = VaultView {
            creators: bk["vault"]["creators"].as_object().unwrap().clone(),
            posts: bk["vault"]["posts"].as_object().unwrap().clone(),
            all_passwords: bk["vault"]["all"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect(),
        };
        let conf = format!("{base}\\config");
        let mut b = Bank::load(&conf, Box::new(|_: &str, _: &str| {}));
        let arg = |a: &Value, k: &str| a[k].as_str().unwrap_or("").replace("<B>", &base).replace('/', "\\");
        for (i, step) in bk["steps"].as_array().unwrap().iter().enumerate() {
            let a = &step["args"];
            let got = match step["op"].as_str().unwrap() {
                "add" => json!(b.add(&arg(a, "password"), &arg(a, "creator"))),
                "add_many" => json!(b.add_many(a["passwords"].as_array().unwrap(), &arg(a, "creator"))),
                "remove" => json!(b.remove(&arg(a, "password"), &arg(a, "creator"))),
                "record" => {
                    b.record(&arg(a, "archive"), &arg(a, "password"), &arg(a, "creator"));
                    Value::Null
                }
                "get_archive" => json!(b.for_archive(&arg(a, "archive"))),
                "passwords" => json!(b.passwords()),
                "by_creator" => b.by_creator(),
                "candidates" => json!(b.candidates(&arg(a, "archive"), &arg(a, "creator"), &vault)),
                "sync" => json!(b.sync_from_vault(&vault)),
                "clear" => json!(b.clear()),
                op => panic!("{op}"),
            };
            let got = serde_json::from_str::<Value>(&got.to_string().replace(&base.replace('\\', "\\\\"), "<B>")).unwrap();
            assert_eq!(got, step["result"], "step {i} {}", step["op"]);
            for (f, want) in step["files"].as_object().unwrap() {
                let t = std::fs::read_to_string(Path::new(&conf).join(f)).unwrap();
                let t = t.replace("\\\\", "\\").replace(&base.to_lowercase(), "<B>").replace('\\', "/");
                assert_eq!(t, want.as_str().unwrap(), "{f} after step {i}");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
