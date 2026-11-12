//! Which posts and files a download keeps: dates, skip words, character names, file types,
//! extensions, sizes and video length. A port of core/filter_engine.py (the filtering half); the
//! test checks it against the Python version's answers in tests/fixtures/filters.json.

use super::py::{self, casefold, splitext};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::sync::OnceLock;

pub const IMAGE_EXTS: &[&str] = &[".jpg", ".jpeg", ".png", ".webp", ".gif", ".bmp", ".svg", ".psd", ".clip", ".sai", ".avif", ".jfif", ".tif", ".tiff", ".heic", ".heif"];
pub const VIDEO_EXTS: &[&str] = &[".mp4", ".mkv", ".webm", ".mov", ".avi", ".m4v", ".flv", ".wmv"];
pub const AUDIO_EXTS: &[&str] = &[".mp3", ".wav", ".flac", ".ogg", ".m4a", ".aac", ".opus"];
pub const ARCHIVE_EXTS: &[&str] = &[".zip", ".rar", ".7z", ".tar", ".gz", ".bz2", ".xz", ".iso"];

// ── Options ─────────────────────────────────────────────────────────────────

/// Download options, in the shape the Python engine saves them (FilterOptions.to_dict()).
#[derive(Debug, Clone)]
pub struct Options(Map<String, Value>);

fn defaults() -> Map<String, Value> {
    serde_json::from_str::<Value>(r#"{
        "characters": "", "character_scope": "title", "skip_words": "", "skip_scope": "posts", "remove_words": "",
        "file_type": "all", "skip_archives": false, "download_thumbnails_only": false, "scan_content_images": true,
        "download_dms": false, "compress_to_webp": false, "keep_duplicates": false, "favorite_mode": false,
        "subfolder_per_post": true, "date_prefix": true,
        "download_revisions": false, "adaptive_threading": false, "threads_locked": false, "auto_retry_at_end": false,
        "filename_style": "post_title", "filename_template": "{title} - {orig_name}", "proxy_url": "",
        "page_start": 1, "page_end": 999999, "download_delay": 2.0, "save_post_metadata": true, "download_embeds": true,
        "file_index_prefix": false, "skip_post_covers": false, "date_after": "", "date_before": "",
        "download_pawchive_temporary_files": true, "min_file_size": "", "max_file_size": "", "write_audio_metadata": false,
        "fallback_to_thumbnails": false, "redownload_small_files": false, "exact_extensions": "", "skip_retry_404": false,
        "group_file_type": "none", "file_order": "posted", "webp_quality": "balanced", "write_sidecars": false,
        "sidecar_format": "json", "sidecar_scope": "per_file"
    }"#)
        .unwrap()
        .as_object()
        .cloned()
        .unwrap()
}

/// Python's truthiness of a JSON value
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl Options {
    /// The settings as FilterOptions.to_dict() gives them
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// FilterOptions.from_dict(d): defaults for what's missing, values normalised
    pub fn from_dict(d: &Value) -> Self {
        let mut m = defaults();
        let Some(src) = d.as_object() else { return Self(m) };
        for (k, v) in src {
            if m.contains_key(k) {
                m.insert(k.clone(), v.clone());
            }
        }
        // the same conversions FilterOptions applies
        let bools = [
            "skip_archives", "download_thumbnails_only", "scan_content_images", "download_dms", "compress_to_webp",
            "keep_duplicates", "favorite_mode", "subfolder_per_post", "date_prefix",
            "download_revisions", "adaptive_threading", "threads_locked", "auto_retry_at_end",
            "save_post_metadata", "download_embeds", "file_index_prefix", "skip_post_covers", "download_pawchive_temporary_files",
            "write_audio_metadata", "fallback_to_thumbnails", "redownload_small_files", "skip_retry_404", "write_sidecars",
        ];
        for k in bools {
            let b = truthy(&m[k]);
            m.insert(k.into(), json!(b));
        }
        for k in ["page_start", "page_end"] {
            let n = match &m[k] {
                Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
                Value::String(s) => py::int(s).unwrap_or(0),
                Value::Bool(b) => *b as i64,
                _ => 0,
            };
            m.insert(k.into(), json!(n));
        }
        let delay = match &m["download_delay"] {
            Value::Number(n) => n.as_f64().unwrap_or(2.0),
            Value::String(s) => py::float(s).unwrap_or(2.0),
            _ => 2.0,
        };
        m.insert("download_delay".into(), json!(delay));
        let tpl = as_text(&m["filename_template"]);
        m.insert("filename_template".into(), json!(if tpl.is_empty() { "{title} - {orig_name}".to_string() } else { tpl }));
        m.insert("exact_extensions".into(), json!(as_text(&m["exact_extensions"]).trim()));
        for k in ["min_file_size", "max_file_size"] {
            let raw = m[k].clone();
            m.insert(k.into(), json!(if truthy(&raw) { normalize_size_str(&as_text(&raw)) } else { String::new() }));
        }
        let pick = |v: &Value, allowed: &[&str], fallback: &str| -> String {
            let s = as_text(v).to_lowercase();
            if allowed.contains(&s.as_str()) { s } else { fallback.to_string() }
        };
        m.insert("group_file_type".into(), json!(pick(&m["group_file_type"], &["none", "post", "creator"], "none")));
        let order = as_text(&m["file_order"]).trim().to_lowercase();
        m.insert("file_order".into(), json!(if ["posted", "reversed", "name"].contains(&order.as_str()) { order } else { "posted".into() }));
        m.insert("webp_quality".into(), json!(pick(&m["webp_quality"], &["lossless", "high", "balanced", "small", "smallest"], "balanced")));
        m.insert("sidecar_format".into(), json!(pick(&m["sidecar_format"], &["json", "txt", "both"], "json")));
        m.insert("sidecar_scope".into(), json!(pick(&m["sidecar_scope"], &["per_file", "per_post"], "per_file")));
        Self(m)
    }

    pub fn to_dict(&self) -> Value {
        Value::Object(self.0.clone())
    }

    pub fn str(&self, k: &str) -> String {
        self.0.get(k).map(as_text).unwrap_or_default()
    }

    pub fn flag(&self, k: &str) -> bool {
        self.0.get(k).is_some_and(truthy)
    }

    pub fn int(&self, k: &str) -> i64 {
        self.0.get(k).and_then(Value::as_i64).unwrap_or(0)
    }
}

// ── Text matching ───────────────────────────────────────────────────────────

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

fn no_space_script() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"[\x{3040}-\x{30ff}\x{3400}-\x{4dbf}\x{4e00}-\x{9fff}\x{f900}-\x{faff}\x{ac00}-\x{d7af}\x{0e00}-\x{0e7f}]"))
}

/// Whole-word match for Latin-script terms; plain substring for Japanese, Chinese, Korean and Thai
pub fn contains_term(text: &str, term: &str) -> bool {
    if term.is_empty() || text.is_empty() {
        return false;
    }
    if no_space_script().is_match(term) {
        return casefold(text).contains(&casefold(term));
    }
    // Whole word as Python's \b sees it: letters, digits and "_" are word characters (combining
    // marks aren't, unlike in the regex crate's \b)
    let Ok(r) = Regex::new(&format!("(?i){}", regex::escape(term))) else { return false };
    let is_word = |c: char| c == '_' || matches!(py::category(c).as_bytes()[0], b'L' | b'N');
    let edge = |outside: Option<char>, inside: Option<char>| outside.is_some_and(is_word) != inside.is_some_and(is_word);
    let mut from = 0;
    while from <= text.len() {
        let Some(m) = r.find_at(text, from) else { break };
        let (before, after) = (text[..m.start()].chars().next_back(), text[m.end()..].chars().next());
        let (first, last) = (m.as_str().chars().next(), m.as_str().chars().next_back());
        if edge(before, first) && edge(after, last) {
            return true;
        }
        // matches may overlap: try again one character later
        from = m.start() + text[m.start()..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    false
}

/// Post content without HTML tags
pub fn plain_text(html: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"<[^>]+>")).replace_all(html, " ").into_owned()
}

fn comma_list(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

// ── Sizes ───────────────────────────────────────────────────────────────────

/// "1g" → "1 GB", "500" → "500 MB"; "" when invalid or 0
pub fn normalize_size_str(s: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    let Some(c) = R.get_or_init(|| re(r"^([\d.]+)\s*([A-Za-z]+)?")).captures(s) else { return String::new() };
    let Some(val) = py::float(&c[1]).filter(|v| *v > 0.0) else { return String::new() };
    let raw = c.get(2).map(|m| m.as_str().to_uppercase()).unwrap_or_else(|| "MB".into());
    let unit = match raw.as_str() {
        "B" | "BYTE" | "BYTES" => "B",
        "K" | "KB" | "KIB" => "KB",
        "M" | "MB" | "MIB" => "MB",
        "G" | "GB" | "GIB" => "GB",
        "T" | "TB" | "TIB" => "TB",
        "P" | "PB" | "PIB" => "PB",
        _ => "MB",
    };
    format!("{} {unit}", py::fmt_g(val))
}

pub fn parse_size_str(s: &str) -> i64 {
    static R: OnceLock<Regex> = OnceLock::new();
    if s.is_empty() {
        return 0;
    }
    let norm = normalize_size_str(s);
    let Some(c) = R.get_or_init(|| re(r"^([\d.]+)\s*(B|KB|MB|GB|TB|PB)$")).captures(&norm) else { return 0 };
    let Some(val) = py::float(&c[1]) else { return 0 };
    let mult: f64 = match &c[2] {
        "B" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024f64.powi(2),
        "GB" => 1024f64.powi(3),
        "TB" => 1024f64.powi(4),
        "PB" => 1024f64.powi(5),
        _ => 1024f64.powi(2),
    };
    (val * mult) as i64
}

/// Size limits written into the skip words: [1GB-2GB], [>=500KB], [<=2GB], [1000]…
pub fn size_range_from_skip_words(skip_words: &str) -> (Option<i64>, Option<i64>) {
    if skip_words.is_empty() {
        return (None, None);
    }
    static BRACKET: OnceLock<Regex> = OnceLock::new();
    let Some(c) = BRACKET.get_or_init(|| re(r"(?i)\[\s*(?:size:)?\s*([^\]]+)\]")).captures(skip_words) else {
        return (None, None);
    };
    let expr = c[1].trim().to_string();
    let unit = r"(?:[KMGTP]i?B|[KMGTP])?";
    if let Some(r) = Regex::new(&format!(r"(?i)^([\d.]+\s*{unit})\s*(?:-|–|\.\.|to|<=)\s*([\d.]+\s*{unit})$")).unwrap().captures(&expr) {
        let (mut a, mut b) = (parse_size_str(&r[1]), parse_size_str(&r[2]));
        if a != 0 && b != 0 && a > b {
            std::mem::swap(&mut a, &mut b);
        }
        return (Some(a), Some(b));
    }
    let gt = Regex::new(&format!(r"(?i)^(?:>=|>|\+)?\s*([\d.]+\s*{unit})\+?$")).unwrap().captures(&expr);
    if expr.starts_with('>') || expr.ends_with('+') {
        return match gt {
            Some(g) => (Some(parse_size_str(&g[1])).filter(|v| *v != 0), None),
            None => (None, None),
        };
    }
    if let Some(l) = Regex::new(&format!(r"(?i)^(?:<=|<)\s*([\d.]+\s*{unit})$")).unwrap().captures(&expr) {
        return (None, Some(parse_size_str(&l[1])));
    }
    if let Some(s) = Regex::new(&format!(r"(?i)^([\d.]+\s*{unit})$")).unwrap().captures(&expr) {
        return (Some(parse_size_str(&s[1])), None);
    }
    (None, None)
}

pub fn format_size_str(n: i64) -> String {
    let f = n as f64;
    if n >= 1024i64.pow(4) {
        format!("{:.2} TB", f / 1024f64.powi(4))
    } else if n >= 1024i64.pow(3) {
        format!("{:.2} GB", f / 1024f64.powi(3))
    } else if n >= 1024i64.pow(2) {
        format!("{:.2} MB", f / 1024f64.powi(2))
    } else if n >= 1024 {
        format!("{:.1} KB", f / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// (min, max) bytes from the size fields, else from the skip words; an inverted range is swapped
pub fn effective_size_limits(o: &Options) -> (Option<i64>, Option<i64>) {
    let (smin, smax) = (o.str("min_file_size").trim().to_string(), o.str("max_file_size").trim().to_string());
    let mut min = (!smin.is_empty()).then(|| parse_size_str(&smin));
    let mut max = (!smax.is_empty()).then(|| parse_size_str(&smax));
    let (wmin, wmax) = size_range_from_skip_words(&o.str("skip_words"));
    if min.is_none() {
        min = wmin;
    }
    if max.is_none() {
        max = wmax;
    }
    if let (Some(a), Some(b)) = (min, max) {
        if a > b {
            return (Some(b), Some(a));
        }
    }
    (min, max)
}

// ── Dates ───────────────────────────────────────────────────────────────────

/// "2024/6/5" → "2024-06-05", "2024.02" → "2024-02", "2024" stays
pub fn normalize_date(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let s = s.trim().replace(['/', '.'], "-");
    let parts: Vec<&str> = s.split('-').collect();
    match parts.len() {
        3 => {
            if let (Some(y), Some(m), Some(d)) = (py::int(parts[0]), py::int(parts[1]), py::int(parts[2])) {
                return format!("{y:04}-{m:02}-{d:02}");
            }
        }
        2 => {
            if let (Some(y), Some(m)) = (py::int(parts[0]), py::int(parts[1])) {
                return format!("{y:04}-{m:02}");
            }
        }
        1 if parts[0].len() == 4 && parts[0].chars().all(|c| c.is_ascii_digit()) => return parts[0].to_string(),
        _ => {}
    }
    py::take_chars(&s, 10)
}

pub fn expand_date_start(s: &str) -> String {
    let n = normalize_date(s);
    match n.split('-').count() {
        _ if n.is_empty() => String::new(),
        1 => format!("{n}-01-01"),
        2 => format!("{n}-01"),
        _ => n,
    }
}

fn days_in_month(y: i64, m: i64) -> Option<i64> {
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    Some(match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    })
}

pub fn expand_date_end(s: &str) -> String {
    let n = normalize_date(s);
    if n.is_empty() {
        return n;
    }
    let parts: Vec<&str> = n.split('-').collect();
    match parts.len() {
        1 => format!("{n}-12-31"),
        2 => match (py::int(parts[0]), py::int(parts[1])) {
            (Some(y), Some(m)) => match days_in_month(y, m) {
                Some(d) => format!("{y:04}-{m:02}-{d:02}"),
                None => format!("{n}-30"),
            },
            _ => format!("{n}-30"),
        },
        _ => n,
    }
}

// ── Extensions and file types ───────────────────────────────────────────────

/// "zip, .rar, *.7z" → {".zip", ".rar", ".7z"}
pub fn parse_extensions_list(raw: &str) -> BTreeSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"[,;\s]+"))
        .split(raw.trim())
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .map(|t| {
            let t = t.trim_start_matches('*').to_string();
            if t.starts_with('.') { t } else { format!(".{t}") }
        })
        .filter(|t| t.chars().count() > 1)
        .collect()
}

/// The folder a file goes to when files are grouped by type
pub fn file_type_category(filename: &str) -> &'static str {
    if filename.is_empty() {
        return "Other";
    }
    let lower = filename.to_lowercase();
    let ext = splitext(&lower).1;
    match ext {
        ".jpg" | ".jpeg" | ".png" | ".webp" | ".gif" | ".bmp" | ".svg" | ".ico" | ".tiff" | ".tif" | ".avif" | ".heic" | ".heif" | ".jfif" => "Images",
        ".mp4" | ".mkv" | ".webm" | ".mov" | ".avi" | ".m4v" | ".flv" | ".wmv" | ".ts" | ".mts" | ".m2ts" | ".3gp" => "Video",
        ".zip" | ".rar" | ".7z" | ".tar" | ".gz" | ".bz2" | ".xz" | ".iso" | ".psd" | ".clip" | ".sai" | ".sai2" | ".cpt" | ".procreate" | ".7zip" | ".tgz" => "Archive",
        ".mp3" | ".wav" | ".flac" | ".ogg" | ".m4a" | ".aac" | ".opus" | ".wma" | ".aiff" | ".alac" | ".mka" => "Audio",
        _ => "Other",
    }
}

// ── The filters ─────────────────────────────────────────────────────────────

fn post_text(post: &Value, k: &str) -> String {
    post.get(k).filter(|v| truthy(v)).map(as_text).unwrap_or_default()
}

/// The post's date as "YYYY-MM-DD" ("" when it has none)
fn post_date(post: &Value) -> String {
    let published = post.get("published").filter(|v| truthy(v)).or_else(|| post.get("added")).cloned().unwrap_or(Value::Null);
    match &published {
        Value::Number(n) => {
            let ts = n.as_f64().unwrap_or(0.0);
            chrono::DateTime::from_timestamp(ts as i64, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| py::take_chars(&n.to_string(), 10))
        }
        v if truthy(v) => {
            let s = as_text(v).trim().to_string();
            if s.contains('T') { s.split('T').next().unwrap_or("").to_string() } else { py::take_chars(&s, 10) }
        }
        _ => String::new(),
    }
}

pub fn should_keep_post(post: &Value, o: &Options) -> (bool, String) {
    let title = post_text(post, "title");
    let content = post_text(post, "content");

    let (after, before) = (o.str("date_after"), o.str("date_before"));
    if !after.is_empty() || !before.is_empty() {
        let date = post_date(post);
        if date.is_empty() || date == "0000-00-00" {
            return (false, "Post has no valid publication date".into());
        }
        let (mut a, mut b) = (expand_date_start(&after), expand_date_end(&before));
        if !a.is_empty() && !b.is_empty() && a > b {
            std::mem::swap(&mut a, &mut b);
        }
        if !a.is_empty() && date < a {
            return (false, format!("Post date ({date}) is before {a}"));
        }
        if !b.is_empty() && date > b {
            return (false, format!("Post date ({date}) is after {b}"));
        }
    }

    let skip = o.str("skip_words");
    if !skip.is_empty() && ["posts", "both"].contains(&o.str("skip_scope").as_str()) {
        let plain = plain_text(&content);
        for word in comma_list(&skip) {
            if word.starts_with('[') && word.ends_with(']') {
                continue;
            }
            if contains_term(&title, &word) || contains_term(&plain, &word) {
                return (false, format!("Post contains skipped word: '{word}'"));
            }
        }
    }

    let characters = o.str("characters");
    if !characters.is_empty() {
        let comments = post_text(post, "comments_text");
        let search = match o.str("character_scope").as_str() {
            "title" => title.clone(),
            "content" => plain_text(&content),
            "comments" => format!("{title}\n{comments}"),
            _ => format!("{title}\n{}\n{comments}", plain_text(&content)),
        };
        let matched = comma_list(&characters).iter().any(|term| {
            let clean = term.trim_matches(['(', ')']);
            let mut subs: Vec<&str> = clean.split('/').map(str::trim).filter(|s| !s.is_empty()).collect();
            if subs.is_empty() {
                subs = vec![clean];
            }
            subs.iter().any(|s| contains_term(&search, s))
        });
        if !matched {
            return (false, format!("Post did not match character filter: '{characters}'"));
        }
    }
    (true, "Passed filter".into())
}

pub fn should_keep_file(filename: &str, o: &Options, file_size: Option<i64>) -> (bool, String) {
    if filename.is_empty() {
        return (false, "Empty filename".into());
    }
    let lower = filename.to_lowercase();
    let ext = splitext(&lower).1;

    if let Some(size) = file_size.filter(|s| *s > 0) {
        let (min, max) = effective_size_limits(o);
        if let Some(m) = min.filter(|m| size < *m) {
            return (false, format!("File size ({}) is below minimum threshold ({})", format_size_str(size), format_size_str(m)));
        }
        if let Some(m) = max.filter(|m| size > *m) {
            return (false, format!("File size ({}) exceeds maximum threshold ({})", format_size_str(size), format_size_str(m)));
        }
    }
    let exact = o.str("exact_extensions");
    if o.flag("skip_archives") && ARCHIVE_EXTS.contains(&ext) {
        let allowed = if exact.is_empty() { BTreeSet::new() } else { parse_extensions_list(&exact) };
        if allowed.is_empty() || !allowed.contains(ext) {
            return (false, "Archive skipped due to 'Skip Archives' setting".into());
        }
    }
    if o.flag("download_thumbnails_only") {
        if ARCHIVE_EXTS.contains(&ext) {
            return (false, format!("Archive skipped (no thumbnails available for {ext})"));
        }
        if AUDIO_EXTS.contains(&ext) {
            return (false, format!("Audio skipped (no thumbnails available for {ext})"));
        }
        if !IMAGE_EXTS.contains(&ext) && !VIDEO_EXTS.contains(&ext) {
            return (false, format!("Non-visual file skipped (no thumbnails available for {ext})"));
        }
    }
    let skip = o.str("skip_words");
    if !skip.is_empty() && ["files", "both"].contains(&o.str("skip_scope").as_str()) {
        let stem = splitext(filename).0;
        for word in comma_list(&skip) {
            if word.starts_with('[') && word.ends_with(']') {
                continue;
            }
            let hit = if no_space_script().is_match(&word) {
                casefold(stem).contains(&casefold(&word))
            } else {
                Regex::new(&format!(r"(?i)(?:^|[_\-\s]){}(?:[_\-\s]|$)", regex::escape(&word))).map(|r| r.is_match(stem)).unwrap_or(false)
            };
            if hit {
                return (false, format!("File contains skipped word: '{word}'"));
            }
        }
    }
    if !exact.is_empty() {
        let allowed = parse_extensions_list(&exact);
        if !allowed.is_empty() && !allowed.contains(ext) {
            let list = allowed.iter().cloned().collect::<Vec<_>>().join(", ");
            return (false, format!("File extension '{ext}' does not match exact extension filter ({list})"));
        }
    } else {
        match o.str("file_type").as_str() {
            "images" if !IMAGE_EXTS.contains(&ext) => return (false, format!("Not an image file ({ext})")),
            "videos" if !VIDEO_EXTS.contains(&ext) => return (false, format!("Not a video file ({ext})")),
            "archives" if !ARCHIVE_EXTS.contains(&ext) => return (false, format!("Not an archive file ({ext})")),
            "audio" if !AUDIO_EXTS.contains(&ext) => return (false, format!("Not an audio file ({ext})")),
            _ => {}
        }
    }
    (true, "Passed file filter".into())
}
