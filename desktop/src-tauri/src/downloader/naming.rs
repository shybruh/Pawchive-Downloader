//! File and folder names: cleaning text for the file system, the file-name styles and custom
//! templates, post titles and captions, pictures linked in post text, tags, long Windows paths
//! and creator folders. A port of the naming half of core/filter_engine.py and of
//! core/folder_naming.py, checked against tests/fixtures/filters.json.

use super::filters::{truthy, Options, IMAGE_EXTS};
use super::py::{self, category, splitext, strip_chars};
use md5::{Digest, Md5};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

macro_rules! cached {
    ($pattern:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| re($pattern))
    }};
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Text made safe for a file or folder name: no tags, no characters Windows refuses, no emoji or
/// symbols, single spaces, not a reserved device name, at most `max_len` characters (0 = no limit).
pub fn clean_filesystem_text(text: &str, max_len: usize, fallback: &str) -> String {
    if text.is_empty() {
        return fallback.to_string();
    }
    let text: String = text.nfc().collect();
    let text = cached!(r"<[^>]+>").replace_all(&text, " ");
    let text = cached!(r"[\r\n\t]+").replace_all(&text, " ");
    let text = cached!(r#"[\\/*?:"<>|\x00-\x1f\x7f]"#).replace_all(&text, "_");
    let text: String = text
        .chars()
        .filter(|&c| {
            let cat = category(c);
            let first = cat.as_bytes()[0];
            if matches!(first, b'L' | b'N' | b'M' | b'P') || cat == "Zs" || "+=~$%^&@#`".contains(c) {
                return true;
            }
            !matches!(cat, "So" | "Sk" | "Sm" | "Sc" | "Cf" | "Co" | "Cs" | "Cc")
        })
        .collect();
    let text = python_whitespace_runs(&text);
    let text = cached!(r"_+").replace_all(&text, "_");
    let mut text = strip_chars(&text, " ._\t\r\n").to_string();
    if RESERVED.contains(&text.to_uppercase().as_str()) {
        text = format!("_{text}_");
    }
    if max_len > 0 && py::char_len(&text) > max_len {
        text = strip_chars(&py::take_chars(&text, max_len), " ._").to_string();
    }
    if text.is_empty() { fallback.to_string() } else { text }
}

/// re.sub(r"\s+", " ", text) with Python's idea of whitespace (str.isspace)
fn python_whitespace_runs(text: &str) -> String {
    let is_space = |c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c);
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for c in text.chars() {
        if is_space(c) {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

/// A downloaded file's name: trailing punctuation dropped, the "remove words" taken out, cleaned
pub fn sanitize_filename(filename: &str, o: &Options) -> String {
    let filename = py::rstrip_chars(filename, ".,;!? \t");
    let (name, ext) = splitext(filename);
    let mut name = name.to_string();
    let remove = o.str("remove_words");
    if !remove.is_empty() {
        for word in remove.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(r) = Regex::new(&format!("(?i){}", regex::escape(word))) {
                name = r.replace_all(&name, "").into_owned();
            }
        }
    }
    let clean_name = clean_filesystem_text(&name, 180, "unnamed_file");
    let mut clean_ext = clean_filesystem_text(ext, 16, "");
    if !clean_ext.is_empty() && !clean_ext.starts_with('.') {
        clean_ext = format!(".{clean_ext}");
    }
    format!("{clean_name}{}", if clean_ext.is_empty() { ext } else { &clean_ext })
}

fn is_generic_title(t: &str) -> bool {
    matches!(t.trim().to_lowercase().as_str(), "" | "untitled" | "post" | "untitled post")
}

#[allow(clippy::too_many_arguments)]
pub struct NameParts<'a> {
    pub original_filename: &'a str,
    pub post_title: &'a str,
    pub post_date: &'a str,
    pub post_index: i64,
    pub file_index: i64,
    pub folder_index: Option<i64>,
    pub post_id: &'a str,
    pub artist: &'a str,
    pub service: &'a str,
    pub user_id: &'a str,
    pub caption: &'a str,
}

/// The file's name under the chosen file-name style (or the custom template)
pub fn format_custom_filename(p: &NameParts, o: &Options) -> String {
    let clean_orig = sanitize_filename(p.original_filename, o);
    let (stem, ext) = splitext(&clean_orig);
    let (stem, ext) = (stem.to_string(), ext.to_string());
    let clean_caption = clean_filesystem_text(p.caption, 100, "");
    let mut title = p.post_title.to_string();
    if is_generic_title(&title) && !clean_caption.is_empty() {
        title = clean_caption.clone();
    }
    let clean_title = clean_filesystem_text(&title, 100, "Post");
    let clean_artist = clean_filesystem_text(p.artist, 80, "Artist");
    let date = py::take_chars(p.post_date, 10);
    let dc: Vec<char> = date.chars().collect();
    let slice = |a: usize, b: usize| if dc.len() >= b { dc[a..b].iter().collect::<String>() } else { String::new() };
    let (year, month, day) = (slice(0, 4), slice(5, 7), slice(8, 10));
    let f_idx = p.folder_index.unwrap_or(p.file_index);

    let style = o.str("filename_style");
    let style = if style.is_empty() { "post_title".to_string() } else { style };
    let mut res = match style.as_str() {
        "custom" => {
            let tpl = o.str("filename_template");
            let tpl = if tpl.is_empty() { "{title} - {orig_name}".to_string() } else { tpl };
            let tpl = cached!(r"\.+(\{ext\})").replace_all(&tpl, "$1").into_owned();
            let display_caption = if !clean_caption.is_empty() { clean_caption.clone() } else if !clean_title.is_empty() { clean_title.clone() } else { "Caption".into() };
            let replacements: [(&str, String); 19] = [
                ("{post_id}", p.post_id.to_string()),
                ("{artist}", clean_artist.clone()),
                ("{service}", p.service.to_string()),
                ("{user_id}", p.user_id.to_string()),
                ("{title}", clean_title.clone()),
                ("{caption}", display_caption.clone()),
                ("{captionText}", display_caption.clone()),
                ("{caption_text}", display_caption.clone()),
                ("{date}", date.clone()),
                ("{year}", year),
                ("{month}", month),
                ("{day}", day),
                ("{orig_name}", clean_orig.clone()),
                ("{name}", stem.clone()),
                ("{ext}", ext.clone()),
                ("{file_index}", format!("{:02}", p.file_index)),
                ("{post_index}", format!("{:03}", p.post_index)),
                ("{seq_idx}", format!("{f_idx:03}")),
                ("{folder_index}", format!("{f_idx:03}")),
            ];
            let mut res = tpl.clone();
            for (k, v) in &replacements {
                res = res.replace(k, v);
            }
            if !tpl.contains("{ext}") && !tpl.contains("{orig_name}") && !ext.is_empty() && !res.to_lowercase().ends_with(&ext.to_lowercase()) {
                res.push_str(&ext);
            }
            if !ext.is_empty() {
                if let Ok(r) = Regex::new(&format!(r"\.+({})$", regex::escape(&ext))) {
                    res = r.replace(&res, "$1").into_owned();
                }
            }
            res = clean_filesystem_text(&res, 220, &clean_orig);
            if !ext.is_empty() && res.to_lowercase().ends_with(&ext.to_lowercase()) {
                let keep = py::char_len(&res) - py::char_len(&ext);
                let stem_part = py::rstrip_chars(&py::take_chars(&res, keep), " ._").to_string();
                if !stem_part.is_empty() {
                    res = format!("{stem_part}{ext}");
                }
            }
            res
        }
        "date_post_title" if !date.is_empty() => format!("{date} - {clean_title} - {clean_orig}"),
        "date_post_title" => format!("{clean_title} - {clean_orig}"),
        "date_based" if !date.is_empty() => format!("{date}_{:03}_{:02}{ext}", p.post_index, p.file_index),
        "date_based" => format!("{:03}_{:02}{ext}", p.post_index, p.file_index),
        "post_title_global_numbering" => format!("{:03}_{clean_title}_{:02}{ext}", p.post_index, p.file_index),
        _ => clean_orig.clone(),
    };
    if o.flag("file_index_prefix") && style != "custom" && f_idx > 0 {
        res = format!("{f_idx:03}_{res}");
    }
    res
}

fn unescape(s: &str) -> String {
    html_escape::decode_html_entities(s).into_owned()
}

/// (title, caption): the post's title, or the first line of its text when it has none
pub fn post_title_and_caption(post: &Value, max_title_len: usize, fallback: &str) -> (String, String) {
    let Some(obj) = post.as_object() else { return (fallback.into(), String::new()) };
    let mut title = match obj.get("title") {
        Some(Value::Null) | None => String::new(),
        Some(v) => {
            let t = match v {
                Value::String(s) => s.trim().to_string(),
                other => other.to_string(),
            };
            let t = cached!(r"(?i)<br\s*/?>").replace_all(&t, " ");
            let t = cached!(r"<[^>]+>").replace_all(&t, " ");
            let t = unescape(&t).trim().to_string();
            if matches!(t.to_lowercase().as_str(), "null" | "none") { String::new() } else { t }
        }
    };
    let raw = ["captionText", "caption", "captionHtml", "content", "text", "description", "body"]
        .iter()
        .find_map(|k| obj.get(*k).filter(|v| truthy(v)))
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    let mut caption = String::new();
    if !raw.is_empty() {
        let c = cached!(r"(?i)<br\s*/?>").replace_all(raw.trim(), "\n");
        let c = cached!(r"<[^>]+>").replace_all(&c, "");
        caption = unescape(&c).trim().to_string();
    }
    if (title.is_empty() || matches!(title.to_lowercase().as_str(), "untitled" | "untitled post" | "post")) && !caption.is_empty() {
        let first = caption.split('\n').next().unwrap_or("").trim().to_string();
        title = if first.is_empty() { String::new() } else { py::take_chars(&first, max_title_len).trim().to_string() };
    }
    if title.is_empty() {
        title = fallback.into();
    }
    (title, caption)
}

/// Full-size picture links in a post's HTML (thumbnail links turned into the full file), each once
pub fn content_images(html: &str) -> Vec<String> {
    if html.is_empty() {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for c in cached!(r#"(?:src|href)=["']([^"']+)["']"#).captures_iter(html) {
        let m = &c[1];
        let clean = m.split('?').next().unwrap_or("").to_lowercase();
        let ext = splitext(&clean).1;
        if !IMAGE_EXTS.contains(&ext) {
            continue;
        }
        let mut url = m.to_string();
        if url.contains("/thumbnail/") {
            url = url.replace("/thumbnail/data/", "/data/").replace("/thumbnail/", "/");
        }
        if url.contains("img.pawchive.pw") {
            url = url.replace("img.pawchive.pw", "file.pawchive.pw");
        } else if cached!(r"(?:img|[a-z0-9]+)\.kemono\.[a-z]+").is_match(&url) {
            url = cached!(r"(?:img|[a-z0-9]+)\.kemono\.[a-z]+").replace_all(&url, "file.pawchive.pw").into_owned();
        } else if cached!(r"(?:img|[a-z0-9]+)\.coomer\.[a-z]+").is_match(&url) {
            url = cached!(r"(?:img|[a-z0-9]+)\.coomer\.[a-z]+").replace_all(&url, "cum.st").into_owned();
        } else if url.contains("img.cum.st") {
            url = url.replace("img.cum.st", "cum.st");
        }
        let key = cached!(r"(?i)/(?:data/)?([0-9a-f]{2}/[0-9a-f]{2}/[^\s?#]+)")
            .captures(&url)
            .map(|k| k[1].to_lowercase())
            .unwrap_or_else(|| py::basename(url.split('?').next().unwrap_or("")).to_lowercase());
        if seen.insert(key) {
            out.push(url);
        }
    }
    out
}

/// Tags in any of the shapes the sites send: a list, a list of objects, "{a,b}" or "a, b"
pub fn normalize_tags(raw: &Value) -> Vec<String> {
    let clean = |s: &str| s.trim().trim_matches(['"', '\'']).to_string();
    match raw {
        Value::Array(items) => items
            .iter()
            .map(|t| match t {
                Value::Object(o) => ["label", "slug", "tag"]
                    .iter()
                    .find_map(|k| o.get(*k).filter(|v| truthy(v)))
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .map(|s| clean(&s))
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) => {
            let mut s = s.trim();
            if s.starts_with('{') && s.ends_with('}') && s.len() >= 2 {
                s = s[1..s.len() - 1].trim();
            }
            if s.is_empty() {
                return Vec::new();
            }
            csv_row(s).iter().filter(|f| !f.trim().is_empty()).map(|f| clean(f)).collect()
        }
        _ => Vec::new(),
    }
}

/// The first row of Python's csv.reader over `s` (quotes with "" escapes)
fn csv_row(s: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut at_start = true;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' && at_start {
            quoted = true;
            at_start = false;
        } else if c == ',' {
            fields.push(std::mem::take(&mut cur));
            at_start = true;
        } else if c == '\n' || c == '\r' {
            break;
        } else {
            cur.push(c);
            at_start = false;
        }
    }
    fields.push(cur);
    fields
}

// ── Long Windows paths ──────────────────────────────────────────────────────

/// Windows refuses paths of 260+ characters unless long paths are on system-wide
pub const WINDOWS_MAX_PATH: usize = 250;

fn short_name(name: &str, keep: usize, keep_ext: bool) -> String {
    let (stem, ext) = if keep_ext { splitext(name) } else { (name, "") };
    let digest: String = Md5::digest(name.as_bytes()).iter().map(|b| format!("{b:02x}")).take(3).collect();
    let keep = (keep as i64 - py::char_len(ext) as i64 - 7).max(8) as usize;
    format!("{}_{digest}{ext}", py::rstrip_chars(&py::take_chars(stem, keep), " ._"))
}

/// A too-long Windows path shortened: first the file name, then the deepest folder names. The
/// shortened parts keep their start and get a short hash, so different names stay different.
pub fn fit_path_for_windows(path: &str, max_len: usize) -> String {
    if !cfg!(windows) && !path.contains('\\') {
        return path.to_string();
    }
    let abs = path.replace('/', "\\");
    if py::char_len(&abs) <= max_len {
        return path.to_string();
    }
    let (folder, name) = match abs.rfind('\\') {
        Some(i) => (abs[..i].to_string(), abs[i + 1..].to_string()),
        None => (String::new(), abs.clone()),
    };
    let mut folder = folder;
    let mut room = max_len as i64 - py::char_len(&folder) as i64 - 1;
    if room < 60 {
        let mut parts: Vec<String> = folder.split('\\').map(str::to_string).collect();
        let mut i = parts.len() as i64 - 1;
        while py::char_len(&parts.join("\\")) as i64 > max_len as i64 - 61 && i > 1 {
            let idx = i as usize;
            if py::char_len(&parts[idx]) > 24 {
                parts[idx] = short_name(&parts[idx], 24, false);
            }
            i -= 1;
        }
        folder = parts.join("\\");
        room = max_len as i64 - py::char_len(&folder) as i64 - 1;
    }
    let name = if py::char_len(&name) as i64 > room { short_name(&name, room.max(0) as usize, true) } else { name };
    format!("{folder}\\{name}")
}

// ── Creator folders ─────────────────────────────────────────────────────────

/// "Artist [patreon]" (the default) or just "Artist" ("Site in Folder Name" off)
pub fn creator_folder(name: &str, service: &str, include_service: bool) -> String {
    if include_service && !service.is_empty() { format!("{name} [{service}]") } else { name.to_string() }
}

/// Every name a creator's folder may have, the current setting's first
pub fn creator_folder_names(name: &str, service: &str, include_service: bool) -> Vec<String> {
    let tagged = if service.is_empty() { name.to_string() } else { format!("{name} [{service}]") };
    let first = creator_folder(name, service, include_service);
    let mut out = vec![first.clone()];
    for n in [tagged, name.to_string()] {
        if n != first {
            out.push(n);
        }
    }
    out
}

pub fn is_creator_folder(folder: &str, name: &str, service: &str, include_service: bool) -> bool {
    let low = folder.to_lowercase();
    creator_folder_names(name, service, include_service).iter().any(|n| low == n.to_lowercase())
}

/// The creator's folder inside `parent`: one that already exists under either name, else a new
/// one named by the setting
pub fn pick_creator_folder(parent: &std::path::Path, name: &str, service: &str, include_service: bool) -> std::path::PathBuf {
    for n in creator_folder_names(name, service, include_service) {
        let p = parent.join(&n);
        if p.is_dir() {
            return p;
        }
    }
    parent.join(creator_folder(name, service, include_service))
}
