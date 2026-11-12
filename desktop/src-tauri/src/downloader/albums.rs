//! The album sites: Bunkr (albums, file pages, download links, signed CDN links), Erome albums and
//! nHentai galleries. A port of services/bunkr_client.py, erome_client.py and nhentai_client.py:
//! each answers (title, files) with files {"url", "filename", ["size"], "headers"}; None for the
//! title when the link couldn't be read.
//!
//! Checked against the Python clients on made-up pages (tests/fixtures/albums.json,
//! tools/golden_albums.py). The app downloads the files and names their folders.

use super::py::pystr;
use super::sites::clean_text;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const SIGN_DEFAULT: &str = "https://glb-apisign.cdn.cr/sign";
const META_URL: &str = "https://dl.bunkr.cr/api/_001_v2";

/// What the clients ask the web: Err when there was no answer at all
pub trait Web: Sync {
    fn get(&self, url: &str, headers: &[(&str, String)], timeout: u64) -> Result<(u16, String), String>;
    fn post_id(&self, url: &str, id: &str, headers: &[(&str, String)], timeout: u64) -> Result<(u16, String), String>;
    fn sleep(&self, secs: f64);
    fn log(&self, level: &str, message: &str);
}

/// The real web
pub struct Net {
    client: reqwest::blocking::Client,
}

impl Net {
    pub fn new() -> Self {
        Net { client: reqwest::blocking::Client::builder().build().unwrap_or_default() }
    }

    fn send(&self, req: reqwest::blocking::RequestBuilder, headers: &[(&str, String)], timeout: u64) -> Result<(u16, String), String> {
        let mut req = req.timeout(Duration::from_secs(timeout));
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp.bytes().map_err(|e| e.to_string())?;
        Ok((status, String::from_utf8_lossy(&body).into_owned()))
    }
}

impl Default for Net {
    fn default() -> Self {
        Self::new()
    }
}

impl Web for Net {
    fn get(&self, url: &str, headers: &[(&str, String)], timeout: u64) -> Result<(u16, String), String> {
        self.send(self.client.get(url), headers, timeout)
    }
    fn post_id(&self, url: &str, id: &str, headers: &[(&str, String)], timeout: u64) -> Result<(u16, String), String> {
        self.send(self.client.post(url).json(&json!({"id": id})), headers, timeout)
    }
    fn sleep(&self, secs: f64) {
        std::thread::sleep(Duration::from_secs_f64(secs));
    }
    fn log(&self, level: &str, message: &str) {
        super::service::log_app(level, message, "bunkr", "");
    }
}

// ── Text and links (Python's urllib / core/text_utils equivalents) ───────────────────────────

/// urllib.parse.quote: everything but letters, digits, "_.-~" and `safe` as %XX (UTF-8)
fn quote_with(s: &str, safe: &str, plus: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if b.is_ascii_alphanumeric() || "_.-~".contains(c) || (b < 128 && safe.contains(c)) {
            out.push(c);
        } else if plus && c == ' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn quote(s: &str) -> String {
    quote_with(s, "/", false)
}

/// urllib.parse.unquote (invalid UTF-8 replaced)
pub fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// parse_qs: the query's values by name, blank ones left out, in order of first appearance
fn parse_qs(q: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for part in q.split('&').filter(|p| !p.is_empty()) {
        let Some((k, v)) = part.split_once('=') else { continue };
        let (k, v) = (unquote(&k.replace('+', " ")), unquote(&v.replace('+', " ")));
        if v.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(n, _)| *n == k) {
            Some((_, vs)) => vs.push(v),
            None => out.push((k, vec![v])),
        }
    }
    out
}

/// (scheme://host, path, query, fragment) of a link
fn split_url(u: &str) -> (String, String, String, String) {
    let (rest, frag) = u.split_once('#').map(|(a, b)| (a, b.to_string())).unwrap_or((u, String::new()));
    let (rest, query) = rest.split_once('?').map(|(a, b)| (a, b.to_string())).unwrap_or((rest, String::new()));
    let (origin, path) = match rest.find("://") {
        Some(i) => match rest[i + 3..].find('/') {
            Some(j) => (rest[..i + 3 + j].to_string(), rest[i + 3 + j..].to_string()),
            None => (rest.to_string(), String::new()),
        },
        None => (String::new(), rest.to_string()),
    };
    (origin, path, query, frag)
}

/// sanitize_filesystem_name: display text made safe for a file or folder name
pub fn sanitize_filesystem_name(name: &str, fallback: &str, max_len: usize) -> String {
    let cleaned = clean_text(name);
    if cleaned.is_empty() {
        return fallback.to_string();
    }
    let safe = re!(r#"[\\/*?:"<>|]"#).replace_all(&cleaned, "_");
    let safe = re!(r"_+").replace_all(&safe, "_");
    let strip = |s: &str, chars: &str| s.trim_matches(|c: char| chars.contains(c)).to_string();
    let mut safe = strip(&safe, " ._\t\r\n");
    if max_len > 0 && safe.chars().count() > max_len {
        safe = strip(&safe.chars().take(max_len).collect::<String>(), " ._");
    }
    if safe.is_empty() { fallback.to_string() } else { safe }
}

/// safe_file_name: one file name that stays in its folder (folder parts dropped, reserved
/// Windows names prefixed)
pub fn safe_file_name(name: &str, fallback: &str) -> String {
    let raw = name.replace('\\', "/");
    let raw = raw.rsplit('/').next().unwrap_or("");
    let safe = sanitize_filesystem_name(raw, fallback, 200);
    let stem = safe.split('.').next().unwrap_or("").to_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved { format!("_{safe}") } else { safe }
}

fn bunkr_name(name: &str) -> String {
    sanitize_filesystem_name(name, "bunkr_file", 180)
}

fn file(url: String, name: String, referer: &str) -> Value {
    json!({"url": url, "filename": name, "size": 0, "headers": {"Referer": referer}})
}

fn text_of(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        other => pystr(other),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    v.is_some_and(super::filters::truthy)
}

// ── Bunkr ─────────────────────────────────────────────────────────────────────────────────────

fn bunkr_headers() -> Vec<(&'static str, String)> {
    vec![("User-Agent", UA.into()), ("Origin", "https://bunkr.cr".into()), ("Referer", "https://bunkr.cr/".into())]
}

fn with(mut h: Vec<(&'static str, String)>, k: &'static str, v: String) -> Vec<(&'static str, String)> {
    match h.iter_mut().find(|(n, _)| *n == k) {
        Some(e) => e.1 = v,
        None => h.push((k, v)),
    }
    h
}

/// The signing service's {"token", "ex"} for a storage path
fn sign(w: &dyn Web, path: &str, sign_url: &str) -> Option<Map<String, Value>> {
    let url = format!("{sign_url}?path={}", quote(path));
    let mut last = String::new();
    for attempt in 0..3 {
        match w.get(&url, &bunkr_headers(), 15) {
            Ok((429, _)) => {
                last = "HTTP 429 (rate limited)".into();
                w.sleep(1.0 * (attempt + 1) as f64);
                continue;
            }
            Ok((200, body)) => match serde_json::from_str::<Value>(&body) {
                Ok(Value::Object(m)) if m.contains_key("token") && m.contains_key("ex") => return Some(m),
                Ok(_) => last = "the answer had no token".into(),
                Err(e) => {
                    last = e.to_string();
                    w.sleep(0.8 * (attempt + 1) as f64);
                }
            },
            Ok((s, _)) => last = format!("HTTP {s}"),
            Err(e) => {
                last = e;
                w.sleep(0.8 * (attempt + 1) as f64);
            }
        }
    }
    w.log("warning", &format!("Bunkr: couldn't sign the download link for {path} ({last})."));
    None
}

fn signed(raw_url: &str, name: &str, sig: &Map<String, Value>) -> String {
    format!("{raw_url}?n={}&token={}&ex={}", quote(name), text_of(&sig["token"]), text_of(&sig["ex"]))
}

/// A file by its id: its details from Bunkr's file API, its storage path signed
fn file_by_id(w: &dyn Web, id: &str, original_name: Option<&str>) -> Option<Value> {
    let h = with(with(with(bunkr_headers(), "Content-Type", "application/json".into()), "Origin", "https://dl.bunkr.cr".into()),
                 "Referer", format!("https://dl.bunkr.cr/file/{id}"));
    let mut meta: Option<Value> = None;
    let mut last = String::new();
    for attempt in 0..3 {
        match w.post_id(META_URL, id, &h, 20) {
            Ok((429, _)) => {
                last = "HTTP 429 (rate limited)".into();
                w.sleep(1.2 * (attempt + 1) as f64);
                continue;
            }
            Ok((200, body)) => match serde_json::from_str::<Value>(&body) {
                Ok(v) => {
                    meta = Some(v);
                    break;
                }
                Err(e) => {
                    last = e.to_string();
                    w.sleep(0.8 * (attempt + 1) as f64);
                }
            },
            Ok((s, _)) => last = format!("HTTP {s}"),
            Err(e) => {
                last = e;
                w.sleep(0.8 * (attempt + 1) as f64);
            }
        }
    }
    let m = match meta {
        Some(Value::Object(m)) if !m.is_empty() => m,
        _ => {
            w.log("debug", &format!("Bunkr: no file details for id {id} ({}).", if last.is_empty() { "empty answer" } else { &last }));
            return None;
        }
    };
    let name = if truthy(m.get("original")) {
        text_of(&m["original"])
    } else {
        original_name.filter(|n| !n.is_empty()).unwrap_or("video.mp4").to_string()
    };
    let name = bunkr_name(&name);
    if !truthy(m.get("mediafiles")) || !truthy(m.get("path")) {
        return None;
    }
    let path = text_of(&m["path"]);
    let sig = sign(w, &path, SIGN_DEFAULT)?;
    let raw = format!("{}{path}", text_of(&m["mediafiles"]));
    Some(file(signed(&raw, &name, &sig), name, "https://bunkr.cr/"))
}

fn first<'a>(r: &Regex, s: &'a str) -> Option<&'a str> {
    r.captures(s).map(|c| c.get(1).map_or("", |m| m.as_str()))
}

/// A file page (bunkr.cr/f/<slug>, /v/, /d/, /i/): its name, then a signed CDN link, the file's
/// download id, or a direct media link
fn file_page(w: &dyn Web, page_url: &str) -> Option<Value> {
    let h = with(bunkr_headers(), "Referer", page_url.to_string());
    let mut content: Option<String> = None;
    let mut last = String::new();
    for attempt in 0..3 {
        match w.get(page_url, &h, 20) {
            Ok((429, _)) => {
                last = "HTTP 429 (rate limited)".into();
                w.sleep(1.2 * (attempt + 1) as f64);
                continue;
            }
            Ok((s, body)) if s < 400 => {
                content = Some(body);
                break;
            }
            Ok((s, _)) => {
                last = format!("HTTPError('HTTP {s}')");
                w.sleep(0.8 * (attempt + 1) as f64);
            }
            Err(e) => {
                last = e;
                w.sleep(0.8 * (attempt + 1) as f64);
            }
        }
    }
    let Some(content) = content.filter(|c| !c.is_empty()) else {
        w.log("debug", &format!("Bunkr: couldn't open the file page {page_url} ({}).", if last.is_empty() { "empty page" } else { &last }));
        return None;
    };
    let raw_name = if let Some(n) = first(re!(r#"var\s+ogname\s*=\s*["'](.*?)["']"#), &content) {
        n.to_string()
    } else if let Some(n) = first(re!(r#"(?i)<meta\s+property=["']og:title["']\s+content=["'](.*?)["']"#), &content) {
        n.to_string()
    } else if let Some(t) = first(re!(r"(?i)<title>(.*?)</title>"), &content) {
        t.split('|').next().unwrap_or("").trim().to_string()
    } else {
        String::new()
    };
    let name = bunkr_name(if raw_name.is_empty() { "video.mp4" } else { &raw_name });

    if let Some(cdn) = first(re!(r#"var\s+jsCDN\s*=\s*["']([^"']+)["']"#), &content) {
        let cdn = cdn.replace(r"\/", "/");
        let sign_url = first(re!(r#"var\s+signUrl\s*=\s*["']([^"']+)["']"#), &content)
            .or_else(|| first(re!(r#"SIGN_SERVICE_URL\s*=\s*["']([^"']+)["']"#), &content))
            .unwrap_or(SIGN_DEFAULT)
            .to_string();
        let path = split_url(&cdn).1;
        if let Some(sig) = sign(w, &path, &sign_url) {
            return Some(file(signed(&cdn, &name, &sig), name, "https://bunkr.cr/"));
        }
    }
    let id = first(re!(r#"id=["']download-btn["'][^>]*data-id=["'](\d+)["']"#), &content)
        .or_else(|| first(re!(r#"href=["']https?://(?:[a-zA-Z0-9_-]+\.)?bunkr\.[a-z0-9]+/file/(\d+)["']"#), &content));
    if let Some(id) = id {
        if let Some(f) = file_by_id(w, id, Some(&name)) {
            return Some(f);
        }
    }
    if let Some(u) = first(re!(r#"(?i)(?:src|href)=["'](https?://[^"']+\.(?:mp4|mkv|webm|mov|avi|zip|rar|7z|jpg|png|jpeg|webp))["']"#), &content) {
        return Some(file(u.to_string(), name, page_url));
    }
    None
}

struct Entry {
    id: Option<String>,
    original: Option<String>,
    slug: Option<String>,
    size: u64,
}

/// A Bunkr link: a signed CDN link, a download link, a file page, or an album / folder
pub fn bunkr(w: &dyn Web, url: &str) -> (Option<String>, Vec<Value>) {
    let clean = url.trim();
    // 1. A CDN link, signed already
    if (clean.contains("cdn.cr") || clean.contains("scdn.st")) && clean.contains("/storage/media/") {
        let q = parse_qs(&split_url(clean).2);
        let n = q.iter().find(|(k, _)| k == "n").map(|(_, v)| v[0].clone()).unwrap_or_default();
        let n = if n.is_empty() { clean.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string() } else { n };
        let name = bunkr_name(&unquote(&n));
        return (Some(name.clone()), vec![file(clean.to_string(), name, "https://bunkr.cr/")]);
    }
    // 2. A download link (dl.bunkr.cr/file/<id>)
    if let Some(id) = first(re!(r"/(?:file)/(\d+)"), clean) {
        if let Some(f) = file_by_id(w, id, None) {
            return (Some(text_of(&f["filename"])), vec![f]);
        }
    }
    // 3. A file page
    if re!(r"/(?:f|v|d|i)/([a-zA-Z0-9_-]+)").is_match(clean) && !clean.contains("/a/") {
        if let Some(f) = file_page(w, clean) {
            return (Some(text_of(&f["filename"])), vec![f]);
        }
    }
    // 4. An album (balbums.st's on bunkr.cr), its file list asked for with advanced=1
    let mut fetch_url = clean.to_string();
    if fetch_url.contains("balbums.st") {
        if let Some(id) = first(re!(r"/a/([a-zA-Z0-9_-]+)"), &fetch_url) {
            fetch_url = format!("https://bunkr.cr/a/{id}");
        }
    }
    let (origin, path, query, frag) = split_url(&fetch_url);
    let mut q = parse_qs(&query);
    match q.iter_mut().find(|(k, _)| k == "advanced") {
        Some((_, v)) => *v = vec!["1".into()],
        None => q.push(("advanced".into(), vec!["1".into()])),
    }
    let qs: Vec<String> = q.iter().flat_map(|(k, vs)| vs.iter().map(move |v| format!("{}={}", quote_with(k, "", true), quote_with(v, "", true)))).collect();
    let mut advanced = format!("{origin}{path}?{}", qs.join("&"));
    if !frag.is_empty() {
        advanced = format!("{advanced}#{frag}");
    }
    let h = with(bunkr_headers(), "Referer", fetch_url.clone());
    let mut content: Option<String> = None;
    for attempt in 0..3 {
        match w.get(&advanced, &h, 25) {
            Ok((429, _)) => {
                w.sleep(1.5 * (attempt + 1) as f64);
                continue;
            }
            Ok((s, body)) if s < 400 => {
                content = Some(body);
                break;
            }
            _ => {
                if attempt == 2 {
                    // (advanced=1 failed: the plain page)
                    if let Ok((200, body)) = w.get(&fetch_url, &h, 25) {
                        content = Some(body);
                    }
                }
                w.sleep(1.0 * (attempt + 1) as f64);
            }
        }
    }
    let Some(content) = content.filter(|c| !c.is_empty()) else {
        w.log("warning", &format!("Bunkr: couldn't open the album page {url}."));
        return (None, Vec::new());
    };
    let raw_title = first(re!(r"(?i)<title>(.*?)</title>"), &content).map(|t| t.split('|').next().unwrap_or("").trim().to_string());
    let title = clean_text(raw_title.as_deref().unwrap_or("Bunkr Album"));
    let title = if title.is_empty() { "Bunkr Album".to_string() } else { title };

    // Media links right in the page (older pages)
    let mut legacy: Vec<String> = Vec::new();
    for c in re!(r#"href=["']((?:https?://[^"']+|/(?:v|d|i)/[^"']+))["']"#).captures_iter(&content) {
        let mut link = c[1].to_string();
        if !link.starts_with("http") {
            link = format!("{}{link}", split_url(&fetch_url).0);
        }
        let low = link.to_lowercase();
        if [".mp4", ".mkv", ".webm", ".jpg", ".png", ".jpeg", ".zip", ".rar", ".7z"].iter().any(|e| low.contains(e)) && !legacy.contains(&link) {
            legacy.push(link);
        }
    }

    // The album's file list (window.albumFiles)
    let mut entries: Vec<Entry> = Vec::new();
    if let Some(list) = first(re!(r"(?s)window\.albumFiles\s*=\s*\[(.*?)\];"), &content) {
        for o in re!(r"(?s)\{([^}]+)\}").captures_iter(list) {
            let o = &o[1];
            let id = first(re!(r"id:\s*(\d+)"), o).map(str::to_string);
            let original = first(re!(r#"original:\s*["']([^"']+)["']"#), o).or_else(|| first(re!(r#"name:\s*["']([^"']+)["']"#), o)).map(str::to_string);
            let slug = first(re!(r#"slug:\s*["']([^"']+)["']"#), o).map(str::to_string);
            let size = first(re!(r"size:\s*(\d+)"), o).and_then(|s| s.parse().ok()).unwrap_or(0);
            if id.is_some() || slug.is_some() {
                entries.push(Entry { id, original, slug, size });
            }
        }
    }
    // …else its cards, else its file links
    if entries.is_empty() && legacy.is_empty() {
        for c in re!(r#"(?s)<div[^>]*class=["'][^"']*theItem[^"']*["'][^>]*>(.*?)</div>\s*</div>"#).captures_iter(&content) {
            let card = &c[1];
            let original = first(re!(r#"class=["'][^"']*theName[^"']*["'][^>]*>([^<]+)</p>"#), card)
                .or_else(|| first(re!(r#"title=["']([^"']+)["']"#), card))
                .map(|s| s.trim().to_string());
            if let Some(link) = first(re!(r#"href=["'](/f/[a-zA-Z0-9_-]+)["']"#), card) {
                entries.push(Entry { id: None, original, slug: link.rsplit('/').next().map(str::to_string), size: 0 });
            }
        }
        if entries.is_empty() {
            for c in re!(r#"href=["']((?:https?://[^"']+|/)(?:f|v)/[a-zA-Z0-9_-]+)["']"#).captures_iter(&content) {
                let slug = c[1].trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
                entries.push(Entry { id: None, original: None, slug: Some(slug), size: 0 });
            }
        }
    }

    // Each file's link (6 at a time, in album order)
    let resolve = |e: &Entry| -> Option<Value> {
        if let Some(id) = &e.id {
            if let Some(mut f) = file_by_id(w, id, e.original.as_deref()) {
                if e.size > 0 {
                    f["size"] = json!(e.size);
                }
                return Some(f);
            }
        }
        if let Some(slug) = &e.slug {
            if let Some(mut f) = file_page(w, &format!("https://bunkr.cr/f/{slug}")) {
                if e.size > 0 {
                    f["size"] = json!(e.size);
                }
                return Some(f);
            }
        }
        let what = e.original.clone().or_else(|| e.slug.clone()).or_else(|| e.id.clone()).unwrap_or_default();
        w.log("warning", &format!("Bunkr: couldn't get a download link for '{what}' (details above)."));
        None
    };
    let results: Vec<Mutex<Option<Value>>> = entries.iter().map(|_| Mutex::new(None)).collect();
    let next = Mutex::new(0usize);
    std::thread::scope(|s| {
        for _ in 0..entries.len().min(6) {
            s.spawn(|| loop {
                let i = {
                    let mut n = next.lock().unwrap();
                    *n += 1;
                    *n - 1
                };
                if i >= entries.len() {
                    break;
                }
                *results[i].lock().unwrap() = resolve(&entries[i]);
            });
        }
    });
    let resolved: Vec<Value> = results.into_iter().filter_map(|m| m.into_inner().unwrap()).collect();
    if !resolved.is_empty() {
        return (Some(title), resolved);
    }
    if !legacy.is_empty() {
        legacy.sort();
        let files = legacy
            .into_iter()
            .enumerate()
            .map(|(i, u)| {
                let last = u.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
                let name = safe_file_name(&unquote(&last), &format!("bunkr_file_{:03}", i + 1));
                file(u, name, &fetch_url)
            })
            .collect();
        return (Some(title), files);
    }
    (Some(title), Vec::new())
}

// ── Erome ─────────────────────────────────────────────────────────────────────────────────────

/// An Erome album (erome.com/a/<id>): its videos and pictures, named <id>_001.<ext>…
pub fn erome(w: &dyn Web, url: &str) -> (Option<String>, Vec<Value>) {
    let Some(id) = first(re!(r"/a/(\w+)"), url).map(str::to_string) else { return (None, Vec::new()) };
    let h = vec![("User-Agent", UA.to_string()), ("Referer", "https://www.erome.com/".to_string())];
    let mut content: Option<String> = None;
    for attempt in 0..3 {
        match w.get(url, &h, 25) {
            Ok((429, _)) => {
                w.sleep(2.0 * (attempt + 1) as f64);
                continue;
            }
            Ok((s, body)) if s < 400 => {
                content = Some(body);
                break;
            }
            _ => {
                if attempt == 2 {
                    return (None, Vec::new());
                }
                w.sleep(1.5 * (attempt + 1) as f64);
            }
        }
    }
    let Some(content) = content.filter(|c| !c.is_empty()) else { return (None, Vec::new()) };
    let fallback = format!("Album_{id}");
    let title = first(re!(r#"property="og:title"\s+content="([^"]+)""#), &content).map(clean_text).unwrap_or_else(|| fallback.clone());
    let safe_title = sanitize_filesystem_name(&title, &fallback, 180);

    let mut urls: Vec<String> = Vec::new();
    for c in re!(r#"<source\s+src="([^"]+)""#).captures_iter(&content) {
        urls.push(c[1].to_string());
    }
    for c in re!(r#"<img[^>]+(?:data-src|src)="([^"]+)"[^>]+class="[^"]*img-front[^"]*""#).captures_iter(&content) {
        urls.push(c[1].to_string());
    }
    for m in re!(r#"(?i)https?://(?:s\d+\.)?erome\.com/[^\s"'<>]+\.(?:mp4|webm|jpg|jpeg|png|webp)"#).find_iter(&content) {
        if !urls.iter().any(|u| u == m.as_str()) {
            urls.push(m.as_str().to_string());
        }
    }
    let mut seen: Vec<String> = Vec::new();
    for u in urls {
        if !seen.contains(&u) && !u.ends_with("thumbnail.jpg") {
            seen.push(u);
        }
    }
    let files = seen
        .into_iter()
        .enumerate()
        .map(|(i, u)| {
            let last = u.split('?').next().unwrap_or("").split('#').next().unwrap_or("").trim_end_matches('/');
            let last = last.rsplit('/').next().unwrap_or("");
            let ext = if last.contains('.') { last.rsplit('.').next().unwrap_or("").to_lowercase() } else { String::new() };
            let ext = if re!(r"^[a-z0-9]{2,5}$").is_match(&ext) { ext } else { "mp4".into() };
            json!({"url": u, "filename": format!("{id}_{:03}.{ext}", i + 1), "headers": {"Referer": url}})
        })
        .collect();
    (Some(format!("Erome - {safe_title} [{id}]")), files)
}

// ── nHentai ───────────────────────────────────────────────────────────────────────────────────

/// An nHentai gallery (its id or link): its pages, named 001.<ext>…
pub fn nhentai(w: &dyn Web, id_or_url: &str) -> (Option<String>, Vec<Value>) {
    let Some(id) = first(re!(r"(\d+)"), id_or_url).map(str::to_string) else { return (None, Vec::new()) };
    let api = format!("https://nhentai.net/api/v2/galleries/{id}");
    let h = vec![("User-Agent", UA.to_string()), ("Accept", "application/json, text/plain, */*".to_string())];
    let mut data: Option<Value> = None;
    for attempt in 0..3 {
        match w.get(&api, &h, 25) {
            Ok((429, _)) => {
                w.sleep(2.0 * (attempt + 1) as f64);
                continue;
            }
            Ok((200, body)) => match serde_json::from_str::<Value>(&body) {
                Ok(v) => {
                    data = Some(v);
                    break;
                }
                Err(_) => {
                    if attempt == 2 {
                        return (None, Vec::new());
                    }
                    w.sleep(1.5 * (attempt + 1) as f64);
                }
            },
            Ok(_) => return (None, Vec::new()),
            Err(_) => {
                if attempt == 2 {
                    return (None, Vec::new());
                }
                w.sleep(1.5 * (attempt + 1) as f64);
            }
        }
    }
    let Some(Value::Object(d)) = data.filter(|d| super::filters::truthy(d)) else { return (None, Vec::new()) };
    let empty = json!({});
    let Some(t) = d.get("title").unwrap_or(&empty).as_object() else { return (None, Vec::new()) };
    let pick = |k: &str| t.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let title = pick("pretty").or_else(|| pick("english")).unwrap_or_else(|| format!("Gallery_{id}"));
    let title = re!(r#"[\\/*?:"<>|]"#).replace_all(&title, "_").trim().to_string();
    let pages = d.get("pages").and_then(Value::as_array).cloned().unwrap_or_default();
    if !truthy(d.get("media_id")) || pages.is_empty() {
        return (None, Vec::new());
    }
    let media = text_of(&d["media_id"]);
    let mut files = Vec::new();
    for (i, p) in pages.iter().enumerate() {
        let Some(p) = p.as_object() else { return (None, Vec::new()) };
        let ext = match p.get("t").and_then(Value::as_str).unwrap_or("j") {
            "p" => "png",
            "w" => "webp",
            "g" => "gif",
            _ => "jpg",
        };
        files.push(json!({"url": format!("https://i.nhentai.net/galleries/{media}/{}.{ext}", i + 1),
                          "filename": format!("{:03}.{ext}", i + 1),
                          "headers": {"Referer": format!("https://nhentai.net/g/{id}/")}}));
    }
    (Some(format!("[{id}] {title}")), files)
}

/// Call "album_files": {"kind": "bunkr" | "erome" | "nhentai", "url"} → {"title", "files"}
pub fn call(a: &Value) -> Result<Value, String> {
    let url = a["url"].as_str().unwrap_or("");
    let net = Net::new();
    let (title, files) = match a["kind"].as_str().unwrap_or("") {
        "bunkr" => bunkr(&net, url),
        "erome" => erome(&net, url),
        "nhentai" => nhentai(&net, url),
        other => return Err(format!("unknown album site {other}")),
    };
    Ok(json!({"title": title, "files": files}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The made-up pages of one case: [method, url, post id, status (or statuses in turn), body]
    struct Pages(Mutex<Vec<Value>>);

    impl Pages {
        fn answer(&self, method: &str, url: &str, id: &str) -> Result<(u16, String), String> {
            let mut pages = self.0.lock().unwrap();
            for p in pages.iter_mut() {
                if p[0] == method && p[1] == url && p[2] == id {
                    let body = p[4].as_str().unwrap_or("").to_string();
                    if let Some(turns) = p[3].as_array_mut() {
                        let s = if turns.len() > 1 { turns.remove(0) } else { turns[0].clone() };
                        return Ok((s.as_u64().unwrap() as u16, body));
                    }
                    return Ok((p[3].as_u64().unwrap() as u16, body));
                }
            }
            Ok((404, String::new()))
        }
    }

    impl Web for Pages {
        fn get(&self, url: &str, _: &[(&str, String)], _: u64) -> Result<(u16, String), String> {
            self.answer("GET", url, "")
        }
        fn post_id(&self, url: &str, id: &str, _: &[(&str, String)], _: u64) -> Result<(u16, String), String> {
            self.answer("POST", url, id)
        }
        fn sleep(&self, _: f64) {}
        fn log(&self, _: &str, _: &str) {}
    }

    #[test]
    fn matches_python_clients() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/albums.json")).unwrap();
        for c in &cases {
            let w = Pages(Mutex::new(c["pages"].as_array().unwrap().clone()));
            let url = c["url"].as_str().unwrap();
            let (title, files) = match c["kind"].as_str().unwrap() {
                "bunkr" => bunkr(&w, url),
                "erome" => erome(&w, url),
                _ => nhentai(&w, url),
            };
            assert_eq!(json!(title), c["title"], "title of {url}");
            assert_eq!(Value::Array(files), c["files"], "files of {url}");
        }
    }

    #[test]
    fn quoting_like_python() {
        assert_eq!(quote("/storage/media/x y+é.mp4"), "/storage/media/x%20y%2B%C3%A9.mp4");
        assert_eq!(unquote("a%20b%zz%C3%A9%"), "a b%zzé%");
        assert_eq!(safe_file_name("..\\..\\CON.txt", "f"), "_CON.txt");
        assert_eq!(safe_file_name("com1", "f"), "_com1");
    }
}
