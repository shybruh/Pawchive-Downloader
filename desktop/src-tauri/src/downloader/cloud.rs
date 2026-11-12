//! Cloud links: MEGA (files and folders, decrypted on the way), Dropbox (files, and folders that
//! come as a zip file) and GoFile (with a GoFile Premium account's API token only: GoFile's own
//! website token is never used). A port of services/cloud_downloader.py, with the same log lines
//! (the app shows them, and turns the "❌" ones into the queue row's error). Google Drive is still
//! downloaded by the app.
//!
//! Checked against the Python downloaders on made-up sites (tests/fixtures/cloud.json,
//! tools/golden_cloud.py).

use super::albums::{parse_qs, quote_plus, split_url};
use super::filters::{should_keep_file, Options};
use super::py::{basename, splitext};
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit, KeyIvInit, StreamCipher};
use base64::Engine;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

const MEGA_API: &str = "https://g.api.mega.co.nz/cs";
pub const GOFILE_NEEDS_PREMIUM: &str = "GoFile only lets apps download with a GoFile Premium account's API token. Add yours in Settings → Accounts, or open the link in your browser.";

/// Every cloud download pauses together (the app's pause button)
pub static PAUSED: AtomicBool = AtomicBool::new(false);

// ── The web ───────────────────────────────────────────────────────────────────────────────────

pub struct Resp {
    pub status: u16,
    /// Header names in lower case
    pub headers: HashMap<String, String>,
    pub body: Box<dyn Read + Send>,
}

impl Resp {
    fn json(mut self) -> Option<Value> {
        let mut s = String::new();
        self.body.read_to_string(&mut s).ok()?;
        serde_json::from_str(&s).ok()
    }
    fn header(&self, k: &str) -> &str {
        self.headers.get(k).map(String::as_str).unwrap_or("")
    }
}

pub trait Web: Sync {
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<Resp, String>;
    fn post(&self, url: &str, body: &Value) -> Result<Resp, String>;
}

pub struct Net {
    client: reqwest::blocking::Client,
}

impl Net {
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(None::<Duration>)
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
            .build()
            .unwrap_or_default();
        Net { client }
    }

    fn resp(r: reqwest::blocking::Response) -> Resp {
        let headers = r.headers().iter().map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string())).collect();
        Resp { status: r.status().as_u16(), headers, body: Box::new(r) }
    }
}

impl Default for Net {
    fn default() -> Self {
        Self::new()
    }
}

impl Web for Net {
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<Resp, String> {
        let mut req = self.client.get(url);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        req.send().map(Self::resp).map_err(|e| e.to_string())
    }
    fn post(&self, url: &str, body: &Value) -> Result<Resp, String> {
        self.client.post(url).timeout(Duration::from_secs(25)).json(body).send().map(Self::resp).map_err(|e| e.to_string())
    }
}

// ── One cloud link's download ─────────────────────────────────────────────────────────────────

pub struct Job<'a> {
    pub web: &'a dyn Web,
    pub log: &'a (dyn Fn(&str) + Sync),
    /// (file, bytes so far, its size, which file, how many)
    pub progress: &'a (dyn Fn(&str, u64, u64, usize, usize) + Sync),
    pub cancel: &'a AtomicBool,
    pub skip_words: String,
    pub options: Option<Options>,
    pub workers: usize,
}

impl Job<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn say(&self, s: &str) {
        (self.log)(s)
    }

    /// Copies a download to `out`, a chunk at a time (pausing, speed limit, stopping when
    /// cancelled); `f` changes each chunk first (MEGA's decryption). Bytes copied.
    fn copy(&self, body: &mut dyn Read, out: &mut std::fs::File, name: &str, start: u64, size: u64, idx: usize, n: usize,
            f: &mut dyn FnMut(&mut [u8])) -> u64 {
        let mut buf = vec![0u8; 16384];
        let mut done = start;
        let mut last = Instant::now();
        loop {
            if self.cancelled() {
                break;
            }
            while PAUSED.load(Ordering::Relaxed) && !self.cancelled() {
                std::thread::sleep(Duration::from_millis(500));
            }
            if self.cancelled() {
                break;
            }
            let k = match body.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(k) => k,
            };
            crate::transfer::limiter::take(k as u64, self.cancel);
            if self.cancelled() {
                break;
            }
            f(&mut buf[..k]);
            if out.write_all(&buf[..k]).is_err() {
                break;
            }
            done += k as u64;
            if last.elapsed() >= Duration::from_secs(1) {
                (self.progress)(name, done, size, idx, n);
                last = Instant::now();
            }
        }
        done
    }

    /// Why a download that stopped isn't complete ("" when it is)
    fn unfinished(&self, downloaded: u64, expected: u64) -> String {
        if self.cancelled() {
            return "cancelled".into();
        }
        if expected > 0 && downloaded < expected {
            return format!("connection ended at {} of {} MB", downloaded / 1048576, expected / 1048576);
        }
        String::new()
    }

    /// should_skip_cloud_file: a file (or its folders) the filters or skip words leave out
    pub fn skip(&self, path: &str, size: i64) -> Option<String> {
        skip_reason(path, &self.skip_words, Some(size), self.options.as_ref())
    }

    /// Runs `f` on each item, `workers` at a time, in order
    fn each<T: Sync>(&self, items: &[T], f: &(dyn Fn(usize, &T) + Sync)) {
        let next = Mutex::new(0usize);
        let workers = self.workers.clamp(1, 16).min(items.len().max(1));
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    if self.cancelled() {
                        break;
                    }
                    let i = {
                        let mut n = next.lock().unwrap();
                        *n += 1;
                        *n - 1
                    };
                    if i >= items.len() {
                        break;
                    }
                    f(i, &items[i]);
                });
            }
        });
    }
}

/// _safe_name: a name from a cloud site as ONE safe path component
pub fn safe_name(name: Option<&str>, fallback: &str) -> String {
    let n = re!(r#"[<>:"/\\|?*\x00-\x1f]"#).replace_all(name.unwrap_or(""), "_");
    let n = n.trim().trim_end_matches(['.', ' ']).to_string();
    let mut n = if n.is_empty() || n.chars().all(|c| c == '.') { fallback.to_string() } else { n };
    let stem = n.split('.').next().unwrap_or("").to_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        n = format!("_{n}");
    }
    n.chars().take(200).collect()
}

fn text(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Bool(b)) => Some(if *b { "True".into() } else { "False".into() }),
        Some(o) => Some(o.to_string()),
    }
}

/// The skip reason of a file (or one of its folders), None when it's kept
pub fn skip_reason(path: &str, skip_words: &str, size: Option<i64>, o: Option<&Options>) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let base = basename(path);
    if let Some(o) = o {
        let (keep, why) = should_keep_file(base, o, size);
        if !keep {
            return Some(why);
        }
    }
    let words: Vec<&str> = skip_words.split(',').map(str::trim).filter(|w| !w.is_empty()).collect();
    let stem = splitext(base).0;
    let parts: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    for w in words {
        if w.starts_with('[') && w.ends_with(']') {
            continue;
        }
        let e = regex::escape(w);
        let Ok(pat) = Regex::new(&format!(r"(?i)(?:^|[_\-\s\./\\]){e}(?:[_\-\s\./\\]|$)")) else { continue };
        let Ok(word) = Regex::new(&format!(r"(?i)\b{e}\b")) else { continue };
        if pat.is_match(stem) || word.is_match(base) {
            return Some(format!("File name matches skip keyword: '{w}'"));
        }
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            if pat.is_match(splitext(part).0) || word.is_match(part) {
                return Some(format!("Parent folder matches skip keyword: '{w}'"));
            }
        }
    }
    None
}

fn inside(base: &Path, p: &Path) -> bool {
    let norm = |x: &Path| x.to_string_lossy().replace('/', "\\").to_lowercase();
    let (b, q) = (norm(base), norm(p));
    q.len() > b.len() && q.starts_with(&b) && q[b.len()..].starts_with('\\') && !q.split('\\').any(|c| c == "..")
}

fn remove_quietly(p: &Path) {
    let _ = std::fs::remove_file(p);
}

fn status_error(r: &Resp) -> Option<String> {
    (r.status >= 400).then(|| format!("HTTP {}", r.status))
}

// ── MEGA ──────────────────────────────────────────────────────────────────────────────────────

fn b64(s: &str) -> Vec<u8> {
    let s: String = s.chars().map(|c| match c {
        '-' => '+',
        '_' => '/',
        c => c,
    }).filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/').collect();
    base64::engine::general_purpose::STANDARD_NO_PAD.decode(s.as_bytes()).unwrap_or_default()
}

fn words(b: &[u8]) -> Option<Vec<u32>> {
    (b.len() % 4 == 0).then(|| b.chunks(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect())
}

fn pack(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_be_bytes()).collect()
}

/// (AES key, IV when the key carries one)
fn parse_key(key_b64: &str) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let b = b64(key_b64);
    let w = words(&b)?;
    match w.len() {
        8 => Some((pack(&[w[0] ^ w[4], w[1] ^ w[5], w[2] ^ w[6], w[3] ^ w[7]]), Some(pack(&[w[4], w[5], 0, 0])))),
        4 => Some((b, None)),
        _ => None,
    }
}

fn file_key(raw: &[u8]) -> Option<Vec<u8>> {
    let w = words(raw).filter(|w| w.len() == 8)?;
    Some(pack(&[w[0] ^ w[4], w[1] ^ w[5], w[2] ^ w[6], w[3] ^ w[7]]))
}

fn aes(key: &[u8]) -> Option<aes::Aes128> {
    (key.len() == 16).then(|| aes::Aes128::new(GenericArray::from_slice(key)))
}

fn ecb_decrypt(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let c = aes(key)?;
    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let mut out = data.to_vec();
    for b in out.chunks_mut(16) {
        c.decrypt_block(GenericArray::from_mut_slice(b));
    }
    Some(out)
}

/// A node's attributes ({"n": name…}): AES-CBC with a zero IV over the zero-padded bytes
fn attributes(enc_b64: &str, key: &[u8]) -> Map<String, Value> {
    let Some(c) = aes(key) else { return Map::new() };
    let mut d = b64(enc_b64);
    d.resize(d.len().div_ceil(16) * 16, 0);
    let mut prev = [0u8; 16];
    for b in d.chunks_mut(16) {
        let mut cur = [0u8; 16];
        cur.copy_from_slice(b);
        c.decrypt_block(GenericArray::from_mut_slice(b));
        for (x, p) in b.iter_mut().zip(prev) {
            *x ^= p;
        }
        prev = cur;
    }
    let s = d.iter().position(|&x| x != 0).map(|i| &d[i..]).unwrap_or(&[]);
    let s = &s[..s.iter().rposition(|&x| x != 0).map(|i| i + 1).unwrap_or(0)];
    let Ok(t) = std::str::from_utf8(s) else { return Map::new() };
    let t = t.strip_prefix("MEGA").unwrap_or(t);
    match serde_json::from_str(t) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// An error number instead of an answer (MEGA's -2, -9…)
fn mega_error(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_array().and_then(|a| a.first()).and_then(Value::as_i64))
}

struct MegaFile {
    handle: String,
    size: u64,
    key: String,
    rel: PathBuf,
}

fn mega_folder(job: &Job, folder_id: &str, folder_key: &str) -> Result<(Option<String>, Vec<MegaFile>), String> {
    let master = parse_key(folder_key).ok_or("Invalid Mega key length")?.0;
    let r = job.web.post(&format!("{MEGA_API}?n={folder_id}"), &json!([{"a": "f", "c": 1, "r": 1}]))?;
    if let Some(e) = status_error(&r) {
        return Err(e);
    }
    let v = r.json().ok_or("the answer wasn't JSON")?;
    if let Some(code) = mega_error(&v) {
        job.say(&format!("❌ Mega API returned error code {code}"));
        return Ok((None, Vec::new()));
    }
    let Some(nodes) = v.as_array().and_then(|a| a.first()).and_then(|f| f.get("f")).and_then(Value::as_array) else {
        job.say("❌ Invalid Mega folder response format");
        return Ok((None, Vec::new()));
    };
    struct Node {
        name: String,
        parent: Option<String>,
        kind: Option<i64>,
        size: u64,
        key: String,
    }
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, Node> = HashMap::new();
    for n in nodes {
        let Some(k) = n.get("k").and_then(Value::as_str).filter(|k| !k.is_empty()) else { continue };
        let Some(h) = n.get("h").map(|h| text(Some(h)).unwrap_or_default()) else { continue };
        let Some(raw) = ecb_decrypt(&master, &b64(k.rsplit(':').next().unwrap_or(""))) else { continue };
        let kind = n.get("t").and_then(Value::as_i64);
        let attr_key = if kind == Some(0) {
            match file_key(&raw) {
                Some(k) => k,
                None => continue,
            }
        } else {
            raw.clone()
        };
        let a = attributes(n.get("a").and_then(Value::as_str).unwrap_or(""), &attr_key);
        let name = safe_name(text(a.get("n")).as_deref(), &format!("node_{h}"));
        if !map.contains_key(&h) {
            order.push(h.clone());
        }
        map.insert(h, Node {
            name,
            parent: n.get("p").and_then(|p| text(Some(p))),
            kind,
            size: n.get("s").and_then(Value::as_u64).unwrap_or(0),
            key: base64::engine::general_purpose::STANDARD.encode(&raw),
        });
    }
    let root = map.get(folder_id).map(|n| n.name.clone()).unwrap_or_else(|| format!("mega_folder_{folder_id}"));
    let mut files = Vec::new();
    for h in &order {
        let n = &map[h];
        if n.kind != Some(0) {
            continue;
        }
        let mut parts = vec![n.name.clone()];
        let mut p = n.parent.clone();
        let mut depth = 0;
        while let Some(pid) = p.filter(|pid| map.contains_key(pid)) {
            if pid == folder_id || depth > 256 {
                break;
            }
            let pn = &map[&pid];
            parts.insert(0, pn.name.clone());
            p = pn.parent.clone();
            depth += 1;
        }
        files.push(MegaFile { handle: h.clone(), size: n.size, key: n.key.clone(), rel: parts.iter().collect() });
    }
    Ok((Some(root), files))
}

/// One MEGA file: downloaded to "<name>.part", decrypted on the way (AES-CTR), continued from
/// where an earlier try stopped
fn mega_file(job: &Job, name: &str, size: u64, url: &str, key: &str, dir: &Path, idx: usize, n: usize) -> Result<bool, String> {
    let name = safe_name(Some(name), "mega_file");
    let fin = dir.join(&name);
    let tmp = PathBuf::from(format!("{}.part", fin.to_string_lossy()));
    if !inside(dir, &fin) {
        job.say(&format!("   [Mega] ⚠️ Skipping '{name}': unsafe file name"));
        return Ok(false);
    }
    let _ = std::fs::create_dir_all(dir);
    if job.cancelled() {
        return Ok(false);
    }
    if size > 0 && std::fs::metadata(&fin).is_ok_and(|m| m.len() == size) {
        job.say(&format!("   [Mega] ℹ️ File '{name}' already exists. Skipping."));
        (job.progress)(&name, size, size, idx, n);
        return Ok(true);
    }
    let (k, iv) = parse_key(key).ok_or("Invalid Mega key length")?;
    let iv = iv.unwrap_or_else(|| vec![0; 16]);
    let start = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    let mut headers = Vec::new();
    if start > 0 {
        headers.push(("Range", format!("bytes={start}-")));
        job.say(&format!("   [Mega] Resuming '{name}' from {} MB...", start / 1024 / 1024));
    } else {
        job.say(&format!("   [Mega] 🔽 Downloading '{name}' ({} MB)...", size / 1024 / 1024));
    }
    // (counter at the start file's block, as the Python downloader set it)
    let mut ctr_iv = iv[..8].to_vec();
    ctr_iv.extend_from_slice(&(start / 16).to_be_bytes());
    let mut cipher = ctr::Ctr64BE::<aes::Aes128>::new(GenericArray::from_slice(&k), GenericArray::from_slice(&ctr_iv));
    let mut r = job.web.get(url, &headers)?;
    if let Some(e) = status_error(&r) {
        return Err(e);
    }
    let mut out = std::fs::OpenOptions::new().create(true).write(true).append(start > 0).truncate(start == 0).open(&tmp).map_err(|e| e.to_string())?;
    let done = job.copy(&mut r.body, &mut out, &name, start, size, idx, n, &mut |b| cipher.apply_keystream(b));
    drop(out);
    let why = job.unfinished(done, size);
    if why == "cancelled" {
        job.say(&format!("   [Mega] Download cancelled for '{name}'."));
        remove_quietly(&tmp);
        return Ok(false);
    }
    if !why.is_empty() {
        job.say(&format!("   [Mega] ❌ '{name}' didn't finish ({why})."));    // (the .part stays: the next try continues it)
        return Ok(false);
    }
    if tmp.exists() {
        remove_quietly(&fin);
        std::fs::rename(&tmp, &fin).map_err(|e| e.to_string())?;
        job.say(&format!("   [Mega] ✅ Completed: '{name}'"));
        (job.progress)(&name, size, size, idx, n);
        return Ok(true);
    }
    Ok(false)
}

pub fn mega(job: &Job, url: &str, target: &Path) -> bool {
    let folder = re!(r"mega(?:\.co)?\.nz/folder/([a-zA-Z0-9]+)#([a-zA-Z0-9_.-]+)").captures(url);
    let file = re!(r"mega(?:\.co)?\.nz/(?:file/|#!)?([a-zA-Z0-9]+)(?:#|!)([a-zA-Z0-9_.-]+)").captures(url);
    if let Some(c) = folder {
        let (id, key) = (c[1].to_string(), c[2].to_string());
        job.say(&format!("   [Mega] Folder detected ({id}). Crawling structure..."));
        let (root, files) = match mega_folder(job, &id, &key) {
            Ok(x) => x,
            Err(e) => {
                job.say(&format!("   [Mega] ❌ Mega folder error: {e}"));
                return false;
            }
        };
        if files.is_empty() {
            job.say(&format!("   [Mega] No files found in folder {id}."));
            return false;
        }
        let root = root.unwrap_or_default();
        let files: Vec<MegaFile> = files
            .into_iter()
            .filter(|f| {
                let rel = f.rel.to_string_lossy().to_string();
                match job.skip(&rel, f.size as i64) {
                    Some(why) => {
                        job.say(&format!("   [Mega] ⏭️ Skipping '{rel}': {why}"));
                        false
                    }
                    None => true,
                }
            })
            .collect();
        if files.is_empty() {
            job.say(&format!("   [Mega] All files in folder '{root}' matched skip filters."));
            return true;
        }
        let workers = job.workers.clamp(1, 16);
        job.say(&format!("   [Mega] Found {} file(s) in folder '{root}'. Starting parallel downloads ({workers} threads)...", files.len()));
        let base = target.join(&root);
        let _ = std::fs::create_dir_all(&base);
        let unfinished = Mutex::new(0usize);
        let processed = Mutex::new(0usize);
        let total = files.len();
        job.each(&files, &|_, f| {
            let rel = f.rel.to_string_lossy().to_string();
            let run = || -> Result<bool, String> {
                let r = job.web.post(&format!("{MEGA_API}?n={id}"), &json!([{"a": "g", "g": 1, "n": f.handle}]))?;
                if let Some(e) = status_error(&r) {
                    return Err(e);
                }
                let v = r.json().ok_or("the answer wasn't JSON")?;
                if mega_error(&v).is_some() {
                    job.say(&format!("   [Mega Worker] ⚠️ Skipping file '{rel}' (API error)"));
                    return Ok(true);
                }
                let url = v.get(0).and_then(|d| d.get("g")).and_then(Value::as_str).ok_or("'g'")?.to_string();
                let dir = match f.rel.parent().filter(|p| !p.as_os_str().is_empty()) {
                    Some(sub) => base.join(sub),
                    None => base.clone(),
                };
                if dir != base && !inside(&base, &dir) {
                    job.say(&format!("   [Mega Worker] ⚠️ Skipping '{rel}': unsafe folder name"));
                    return Ok(true);
                }
                let name = f.rel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let idx = *processed.lock().unwrap() + 1;
                mega_file(job, &name, f.size, &url, &f.key, &dir, idx, total)
            };
            match run() {
                Ok(true) => {}
                Ok(false) => *unfinished.lock().unwrap() += 1,
                Err(e) => {
                    *unfinished.lock().unwrap() += 1;
                    if !job.cancelled() {
                        job.say(&format!("   [Mega Worker] ❌ Error on '{rel}': {e}"));
                    }
                }
            }
            *processed.lock().unwrap() += 1;
        });
        if job.cancelled() {
            return false;
        }
        let u = *unfinished.lock().unwrap();
        if u > 0 {
            job.say(&format!("   [Mega] ❌ {u} of {total} file(s) didn't finish."));
            return false;
        }
        return true;
    }
    if let Some(c) = file {
        let (id, key) = (c[1].to_string(), c[2].to_string());
        let run = || -> Result<bool, String> {
            let r = job.web.post(MEGA_API, &json!([{"a": "g", "g": 1, "p": id}]))?;
            if let Some(e) = status_error(&r) {
                return Err(e);
            }
            let v = r.json().ok_or("the answer wasn't JSON")?;
            if mega_error(&v).is_some() {
                job.say(&format!("❌ Mega API returned error code for file {id}"));
                return Ok(false);
            }
            let d = v.get(0).cloned().unwrap_or(Value::Null);
            let url = d.get("g").and_then(Value::as_str).ok_or("'g'")?.to_string();
            let size = d.get("s").and_then(Value::as_u64).unwrap_or(0);
            let k = parse_key(&key).ok_or("Invalid Mega key length")?.0;
            let a = attributes(d.get("at").and_then(Value::as_str).unwrap_or(""), &k);
            let name = match a.get("n") {
                Some(n) => text(Some(n)).unwrap_or_default(),
                None => format!("mega_{id}.bin"),
            };
            if let Some(why) = job.skip(&name, size as i64) {
                job.say(&format!("   [Mega] ⏭️ Skipping '{name}': {why}"));
                return Ok(true);
            }
            mega_file(job, &name, size, &url, &key, target, 1, 1)
        };
        return match run() {
            Ok(ok) => ok,
            Err(e) => {
                job.say(&format!("   [Mega] ❌ Mega error: {e}"));
                false
            }
        };
    }
    job.say(&format!("⚠️ Unrecognized Mega URL format: {url}"));
    false
}

// ── Dropbox ───────────────────────────────────────────────────────────────────────────────────

/// Every file under `dir`, deepest folders first (os.walk(topdown=False)), names sorted
fn walk_bottom_up(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for e in entries.iter().filter(|e| e.is_dir()) {
        walk_bottom_up(e, out);
    }
    out.extend(entries.into_iter().filter(|e| e.is_file()));
}

pub fn dropbox(job: &Job, url: &str, target: &Path) -> bool {
    let _ = std::fs::create_dir_all(target);
    let (origin, path, query, frag) = split_url(url);
    let mut q = parse_qs(&query);
    match q.iter_mut().find(|(k, _)| k == "dl") {
        Some((_, v)) => *v = vec!["1".into()],
        None => q.push(("dl".into(), vec!["1".into()])),
    }
    let qs: Vec<String> = q.iter().flat_map(|(k, vs)| vs.iter().map(move |v| format!("{}={}", quote_plus(k), quote_plus(v)))).collect();
    let mut direct = format!("{origin}{path}?{}", qs.join("&"));
    if !frag.is_empty() {
        direct = format!("{direct}#{frag}");
    }
    job.say(&format!("   [Dropbox] Connecting to direct stream: {direct}"));
    let run = || -> Result<bool, String> {
        let mut r = job.web.get(&direct, &[])?;
        if let Some(e) = status_error(&r) {
            return Err(e);
        }
        let cd = r.header("content-disposition").to_string();
        let name = match re!(r#"filename="?([^"]+)"?"#).captures(&cd) {
            Some(c) => c[1].trim().to_string(),
            None => {
                let b = basename(&path).to_string();
                if b.is_empty() { "dropbox_download".into() } else { b }
            }
        };
        let mut name = safe_name(Some(&name), "dropbox_download");
        if splitext(&name).1.is_empty() {
            name.push_str(".zip");
        }
        let total: u64 = r.header("content-length").trim().parse().unwrap_or(0);
        if let Some(why) = job.skip(&name, total as i64) {
            job.say(&format!("   [Dropbox] ⏭️ Skipping '{name}': {why}"));
            return Ok(true);
        }
        let full = target.join(&name);
        let mut out = std::fs::File::create(&full).map_err(|e| e.to_string())?;
        let done = job.copy(&mut r.body, &mut out, &name, 0, total, 1, 1, &mut |_| {});
        drop(out);
        let why = job.unfinished(done, total);
        if !why.is_empty() {
            remove_quietly(&full);
            job.say(&if why == "cancelled" { "   [Dropbox] Download cancelled.".to_string() } else { format!("   [Dropbox] ❌ '{name}' didn't finish ({why}).") });
            return Ok(false);
        }
        job.say(&format!("   [Dropbox] ✅ Downloaded '{name}'"));
        // A folder comes as a zip file: unpacked next to it (files the filters leave out removed)
        let archive = std::fs::File::open(&full).ok().and_then(|f| zip::ZipArchive::new(f).ok());
        if let Some(mut z) = archive {
            job.say(&format!("   [Dropbox] 📦 Extracting zip archive: '{name}'..."));
            let into = target.join(splitext(&name).0);
            let _ = std::fs::create_dir_all(&into);
            for i in 0..z.len() {
                let Ok(mut e) = z.by_index(i) else { continue };
                let Some(rel) = e.enclosed_name() else { continue };
                let dest = into.join(rel);
                if e.is_dir() {
                    let _ = std::fs::create_dir_all(&dest);
                    continue;
                }
                if let Some(p) = dest.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                if let Ok(mut f) = std::fs::File::create(&dest) {
                    let _ = std::io::copy(&mut e, &mut f);
                }
            }
            drop(z);
            job.say(&format!("   [Dropbox] ✅ Extracted to '{}'", into.to_string_lossy()));
            remove_quietly(&full);
            let mut files = Vec::new();
            walk_bottom_up(&into, &mut files);
            for f in files {
                let rel = f.strip_prefix(&into).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
                let size = std::fs::metadata(&f).map(|m| m.len() as i64).unwrap_or(0);
                if let Some(why) = job.skip(&rel, size) {
                    if std::fs::remove_file(&f).is_ok() {
                        job.say(&format!("   [Dropbox] ⏭️ Pruned extracted '{rel}': {why}"));
                    }
                }
            }
        }
        Ok(true)
    };
    match run() {
        Ok(ok) => ok,
        Err(e) => {
            job.say(&format!("   [Dropbox] ❌ Download error: {e}"));
            false
        }
    }
}

// ── GoFile ────────────────────────────────────────────────────────────────────────────────────

/// A GoFile folder or file, through GoFile's official API with a Premium account's token
pub fn gofile(job: &Job, url: &str, target: &Path, token: &str) -> bool {
    let Some(id) = re!(r"gofile\.io/d/([^/?#]+)").captures(url).map(|c| c[1].to_string()) else {
        job.say(&format!("   [GoFile] ❌ Invalid GoFile URL format: {url}"));
        return false;
    };
    if token.is_empty() {
        job.say(&format!("   [GoFile] ❌ {GOFILE_NEEDS_PREMIUM}"));
        return false;
    }
    let auth = vec![("Authorization", format!("Bearer {token}")), ("Cookie", format!("accountToken={token}"))];
    let run = || -> Result<bool, String> {
        let mut children: Vec<(String, Value)> = Vec::new();
        let mut info: Map<String, Value>;
        let mut page = 1;
        loop {
            let api = format!("https://api.gofile.io/contents/{id}?page={page}&pageSize=1000&sortField=createTime&sortDirection=1");
            let r = job.web.get(&api, &auth)?;
            let code = r.status;
            let data = r.json().unwrap_or(json!({}));
            let status = data.get("status").and_then(Value::as_str).unwrap_or("").to_string();
            match status.as_str() {
                "error-notPremium" => {
                    job.say("   [GoFile] ❌ This GoFile account isn't Premium: GoFile only lets apps read folders with a Premium account. Open the link in your browser instead.");
                    return Ok(false);
                }
                "error-notFound" => {
                    job.say("   [GoFile] ❌ This GoFile link doesn't exist (or was deleted).");
                    return Ok(false);
                }
                "error-passwordRequired" | "error-passwordWrong" => {
                    job.say("   [GoFile] ❌ This GoFile folder has a password. Open it in your browser.");
                    return Ok(false);
                }
                _ => {}
            }
            if code == 401 || status == "error-auth" || status == "error-token" {
                job.say("   [GoFile] ❌ GoFile didn't accept your API token. Copy it again from gofile.io/myprofile.");
                return Ok(false);
            }
            if code != 200 || status != "ok" {
                let what = if status.is_empty() { format!("HTTP {code}") } else { status };
                job.say(&format!("   [GoFile] ❌ GoFile answered {what}."));
                return Ok(false);
            }
            info = data.get("data").and_then(Value::as_object).cloned().ok_or("'data'")?;
            if info.get("type").and_then(Value::as_str) == Some("file") {
                let fid = text(info.get("id")).unwrap_or_else(|| id.clone());
                children = vec![(fid, Value::Object(info.clone()))];
                break;
            }
            let batch: Vec<(String, Value)> = info.get("children").and_then(Value::as_object).map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
            let n = batch.len();
            for (k, v) in batch {
                match children.iter_mut().find(|(c, _)| *c == k) {
                    Some(e) => e.1 = v,
                    None => children.push((k, v)),
                }
            }
            if n < 1000 {
                break;
            }
            page += 1;
        }
        let mut files: Vec<Value> = children.into_iter().map(|(_, v)| v).filter(|c| c.get("type").and_then(Value::as_str) == Some("file")).collect();
        if files.is_empty() {
            job.say("   [GoFile] ℹ️ No files in folder.");
            return Ok(false);
        }
        files.retain(|f| {
            let name = text(f.get("name")).unwrap_or_default();
            match job.skip(&name, f.get("size").and_then(Value::as_i64).unwrap_or(0)) {
                Some(why) => {
                    job.say(&format!("   [GoFile] ⏭️ Skipping '{name}': {why}"));
                    false
                }
                None => true,
            }
        });
        if files.is_empty() {
            job.say("   [GoFile] All files in folder matched skip filters.");
            return Ok(true);
        }
        let workers = job.workers.clamp(1, 16);
        let folder = safe_name(text(info.get("name")).as_deref(), &format!("gofile_{id}"));
        let save = target.join(&folder);
        let _ = std::fs::create_dir_all(&save);
        job.say(&format!("   [GoFile] Found {} file(s). Starting parallel downloads ({workers} threads)...", files.len()));
        let total = files.len();
        let unfinished = Mutex::new(0usize);
        let processed = Mutex::new(0usize);
        job.each(&files, &|_, f| {
            let name = safe_name(text(f.get("name")).as_deref(), &format!("gofile_file_{}", text(f.get("id")).unwrap_or_default()));
            let link = text(f.get("link")).unwrap_or_default();
            let size = f.get("size").and_then(Value::as_u64).unwrap_or(0);
            let path = save.join(&name);
            let one = || -> Result<(), String> {
                if !inside(&save, &path) {
                    job.say(&format!("   [GoFile] ⚠️ Skipping '{name}': unsafe file name"));
                    return Ok(());
                }
                job.say(&format!("   [GoFile] 🔽 '{name}'"));
                let mut r = job.web.get(&link, &auth)?;
                if let Some(e) = status_error(&r) {
                    return Err(e);
                }
                let mut out = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                let idx = *processed.lock().unwrap() + 1;
                let done = job.copy(&mut r.body, &mut out, &name, 0, size, idx, total, &mut |_| {});
                drop(out);
                let why = job.unfinished(done, size);
                if !why.is_empty() {
                    remove_quietly(&path);
                    *unfinished.lock().unwrap() += 1;
                    if why != "cancelled" {
                        job.say(&format!("   [GoFile] ❌ '{name}' didn't finish ({why})."));
                    }
                    return Ok(());
                }
                job.say(&format!("   [GoFile] ✅ Finished '{name}'"));
                (job.progress)(&name, size, size, idx, total);
                Ok(())
            };
            if let Err(e) = one() {
                if job.cancelled() {
                    remove_quietly(&path);
                } else {
                    job.say(&format!("   [GoFile] ❌ Error on '{name}': {e}"));
                }
            }
            *processed.lock().unwrap() += 1;
        });
        if job.cancelled() {
            return Ok(false);
        }
        let u = *unfinished.lock().unwrap();
        if u > 0 {
            job.say(&format!("   [GoFile] ❌ {u} of {total} file(s) didn't finish."));
            return Ok(false);
        }
        Ok(true)
    };
    match run() {
        Ok(ok) => ok,
        Err(e) => {
            job.say(&format!("   [GoFile] ❌ GoFile error: {e}"));
            false
        }
    }
}

/// Call "cloud_download": {"kind": "mega" | "dropbox" | "gofile", "url", "target", "workers",
/// "skip_words", "options", "token"} → whether everything was downloaded. Log lines and progress
/// come back as progress notes ({"log": text} / {"file", "done", "size", "index", "count"}).
pub fn call(id: u64, a: &Value) -> Result<Value, String> {
    let cancel = super::service::cancel_flag(id);
    let limit = super::service::ask_app("limit", json!([])).ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    crate::transfer::limiter::set(limit);
    let log = move |s: &str| super::service::progress_value(id, json!({"log": s}));
    let progress = move |f: &str, done: u64, size: u64, i: usize, n: usize| {
        super::service::progress_value(id, json!({"file": f, "done": done, "size": size, "index": i, "count": n}))
    };
    let net = Net::new();
    let job = Job {
        web: &net,
        log: &log,
        progress: &progress,
        cancel: &cancel,
        skip_words: a["skip_words"].as_str().unwrap_or("").to_string(),
        options: a.get("options").filter(|o| o.is_object()).map(Options::from_dict),
        workers: a["workers"].as_u64().unwrap_or(4) as usize,
    };
    let target = PathBuf::from(a["target"].as_str().unwrap_or(""));
    let url = a["url"].as_str().unwrap_or("");
    let ok = match a["kind"].as_str().unwrap_or("") {
        "mega" => mega(&job, url, &target),
        "dropbox" => dropbox(&job, url, &target),
        "gofile" => gofile(&job, url, &target, a["token"].as_str().unwrap_or("")),
        other => return Err(format!("unknown cloud host {other}")),
    };
    Ok(json!(ok))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The made-up sites of one case: [method, url, payload, status, headers, body (base64)]
    struct Site(Vec<Value>);

    impl Site {
        fn answer(&self, method: &str, url: &str, payload: Option<&Value>) -> Resp {
            for s in &self.0 {
                if s[0] == method && s[1] == url && (s[2].is_null() || Some(&s[2]) == payload) {
                    let body = base64::engine::general_purpose::STANDARD.decode(s[5].as_str().unwrap()).unwrap();
                    let headers = s[4].as_object().unwrap().iter().map(|(k, v)| (k.to_lowercase(), v.as_str().unwrap().to_string())).collect();
                    return Resp { status: s[3].as_u64().unwrap() as u16, headers, body: Box::new(std::io::Cursor::new(body)) };
                }
            }
            Resp { status: 404, headers: HashMap::new(), body: Box::new(std::io::empty()) }
        }
    }

    impl Web for Site {
        fn get(&self, url: &str, _: &[(&str, String)]) -> Result<Resp, String> {
            Ok(self.answer("GET", url, None))
        }
        fn post(&self, url: &str, body: &Value) -> Result<Resp, String> {
            Ok(self.answer("POST", url, Some(body)))
        }
    }

    fn tree(base: &Path) -> Map<String, Value> {
        let mut files = Vec::new();
        walk_bottom_up(base, &mut files);
        files
            .into_iter()
            .map(|f| {
                let rel = f.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                (rel, json!(base64::engine::general_purpose::STANDARD.encode(std::fs::read(&f).unwrap())))
            })
            .collect()
    }

    #[test]
    fn matches_python_downloaders() {
        let fx: Value = serde_json::from_str(include_str!("../../tests/fixtures/cloud.json")).unwrap();
        for s in fx["safe_names"].as_array().unwrap() {
            assert_eq!(safe_name(s[0].as_str(), "fb"), s[1].as_str().unwrap(), "{s}");
        }
        for s in fx["skips"].as_array().unwrap() {
            let o = Options::from_dict(&s[2]);
            let got = skip_reason(s[0].as_str().unwrap(), &o.str("skip_words"), s[1].as_i64(), Some(&o));
            let want = if s[3][0].as_bool().unwrap() { Some(s[3][1].as_str().unwrap().to_string()) } else { None };
            assert_eq!(got, want, "{s}");
        }
        let opts = Options::from_dict(&fx["options"]);
        for (i, c) in fx["cases"].as_array().unwrap().iter().enumerate() {
            let site = Site(c["site"].as_array().unwrap().clone());
            let logs = Mutex::new(Vec::<String>::new());
            let log = |s: &str| logs.lock().unwrap().push(s.to_string());
            let cancel = AtomicBool::new(false);
            let job = Job { web: &site, log: &log, progress: &|_, _, _, _, _| {}, cancel: &cancel, skip_words: opts.str("skip_words"),
                            options: Some(opts.clone()), workers: 1 };
            let base = std::env::temp_dir().join(format!("pawchive_cloud_{}_{i}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let target = base.join("dl");
            let url = c["url"].as_str().unwrap();
            let ok = match c["kind"].as_str().unwrap() {
                "mega" => mega(&job, url, &target),
                "dropbox" => dropbox(&job, url, &target),
                _ => gofile(&job, url, &target, c["token"].as_str().unwrap()),
            };
            // (the Python run's temporary folder in its log lines: this one's)
            let want_logs: Vec<String> = c["logs"].as_array().unwrap().iter().map(|l| {
                let l = l.as_str().unwrap().to_string();
                match l.find("Extracted to '") {
                    Some(p) => format!("{}{}\\folder'", &l[..p + 14], target.to_string_lossy()),
                    None => l,
                }
            }).collect();
            assert_eq!(*logs.lock().unwrap(), want_logs, "logs of {url}");
            assert_eq!(ok, c["ok"].as_bool().unwrap(), "result of {url}");
            assert_eq!(Value::Object(tree(&target)), c["files"], "files of {url}");
            let _ = std::fs::remove_dir_all(&base);
        }
    }
}
