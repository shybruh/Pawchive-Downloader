//! The post picker ("Select posts…") for the archive sites (the bridge's fetchPostsForSelection and
//! startDownloadSelectedPosts; core/post_selection.py): the scan, the preview cards of its posts,
//! and, once posts (and maybe some of their files) are ticked, the plan of those. The scanned posts
//! stay here between the two.
//!
//! Call "selection_scan": {"link", "site", "page_start", "page_end", "date_after", "date_before",
//! "dms"} → {"status": "cancelled" | "no_posts" | "cards", "creator_name", "cards"} (progress notes
//! for the post pages)
//! Call "selection_plan": {"ids", "files", "options", "archive", "archive_db", "base_dir",
//! "include_service", "cookie"} → {"status": "gone" | "empty" | "cancelled" | "plan", "posts", "plan"}

use super::filters::truthy;
use super::links::ParsedLink;
use super::naming::post_title_and_caption;
use super::plan::norm_rel_key;
use super::py::{basename, html_unescape, pystr, splitext};
use super::service;
use super::sites::clean_text;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

const IMAGE_EXTS: [&str; 7] = [".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".avif"];
const VIDEO_EXTS: [&str; 5] = [".mp4", ".m4v", ".webm", ".mov", ".mkv"];

/// `o[k] or ""` as text
fn text(o: &Value, k: &str) -> String {
    o.get(k).filter(|v| truthy(v)).map(pystr).unwrap_or_default()
}

fn is_image(p: &str) -> bool {
    let l = p.to_lowercase();
    let l = l.split('?').next().unwrap_or("");
    !p.is_empty() && IMAGE_EXTS.iter().any(|e| l.ends_with(e))
}

fn data_path(path: &str) -> String {
    let p = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    match p.strip_prefix("/data/") {
        Some(r) => format!("/{r}"),
        None => p,
    }
}

fn thumb(path: &str, domain: &str) -> String {
    format!("https://img.{domain}/thumbnail/data{}", data_path(path))
}

fn video_url(f: &Value, path: &str, name: &str, domain: &str) -> String {
    let lp = path.to_lowercase();
    let lp = lp.split('?').next().unwrap_or("");
    let ln = name.to_lowercase();
    if !(VIDEO_EXTS.iter().any(|e| ln.ends_with(e) || lp.ends_with(e)) || text(f, "mimeType").starts_with("video/")) {
        return String::new();
    }
    let temp = text(f, "temp_url");
    if !temp.is_empty() {
        return temp;
    }
    if path.starts_with("http") {
        return path.to_string();
    }
    let sk = text(f, "storageKey");
    if !sk.is_empty() && domain.contains("cum") {
        let ext = splitext(name).1.to_lowercase();
        return format!("https://cdn.cum.st/media/{sk}/original{}", if ext.is_empty() { ".mp4" } else { &ext });
    }
    if !path.is_empty() && domain.contains("pawchive") {
        return format!("https://file.pawchive.pw/data{}", data_path(path));
    }
    String::new()
}

fn file_thumb(f: &Value, path: &str, name: &str, domain: &str) -> String {
    if !path.is_empty() && (is_image(path) || is_image(name)) {
        return thumb(path, domain);
    }
    let sk = text(f, "storageKey");
    if !sk.is_empty() && (domain.contains("cum.st") || domain.contains("cumst")) && (is_image(name) || text(f, "mimeType").starts_with("image/")) {
        let ext = splitext(name).1.to_lowercase();
        return format!("https://cdn.cum.st/media/{sk}/original{}", if ext.is_empty() { ".jpg" } else { &ext });
    }
    String::new()
}

/// `o["name"] or o["originalFilename"]` (cum.st files have only the latter)
fn file_name(o: &Value) -> String {
    let n = text(o, "name");
    if n.is_empty() { text(o, "originalFilename") } else { n }
}

/// A file of a card; cum.st files have no path: their storage key names them for picking
fn entry(f: &Value, name: String, path: String, th: String, video: String, main: bool) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("name".into(), json!(name));
    m.insert("path".into(), json!(if path.is_empty() { text(f, "storageKey") } else { path }));
    m.insert("thumbnail".into(), json!(th));
    m.insert("previewUrl".into(), json!(th));
    m.insert("videoUrl".into(), json!(video));
    m.insert("is_main".into(), json!(main));
    m.insert("selected".into(), json!(true));
    m
}

/// One post's preview card: title, text, date, thumbnail and its files
pub fn card(p: &Value, domain: &str) -> Value {
    let pid = p.get("id").map(pystr).unwrap_or_default();
    let (title, _) = post_title_and_caption(p, 100, "Untitled Post");
    let published: String = text(p, "published").chars().take(10).collect();
    let f = p.get("file").filter(|f| f.is_object() && truthy(f));
    let atts: Vec<&Value> = p.get("attachments").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();

    let mut thumb_url = String::new();
    if let Some(f) = f {
        let (fp, fname) = (text(f, "path"), text(f, "name"));
        if !fp.is_empty() && (is_image(&fp) || is_image(&fname)) {
            thumb_url = thumb(&fp, domain);
        }
    }
    if thumb_url.is_empty() {
        for a in atts.iter().filter(|a| a.is_object()) {
            let ap = text(a, "path");
            if ap.is_empty() {
                continue;
            }
            if is_image(&ap) || is_image(&text(a, "name")) {
                thumb_url = thumb(&ap, domain);
                break;
            }
        }
    }

    let mut files: Vec<Map<String, Value>> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let key_of = |o: &Value, path: &str| {
        let k = norm_rel_key(o);
        if k.is_empty() { path.to_lowercase() } else { k }
    };
    // 1. The main file
    if let Some(f) = f.filter(|f| truthy(&f["path"]) || truthy(&f["storageKey"])) {
        let path = text(f, "path");
        let mut name = file_name(f);
        if name.is_empty() {
            name = basename(if path.is_empty() { "main_file" } else { &path }).to_string();
        }
        let k = key_of(f, &path);
        if !k.is_empty() {
            seen.insert(k);
        }
        let th = file_thumb(f, &path, &name, domain);
        let video = if th.is_empty() { video_url(f, &path, &name, domain) } else { String::new() };
        files.push(entry(f, name, path, th, video, true));
    }
    // 2. The attachments
    for a in atts.iter().filter(|a| a.is_object() && (truthy(&a["path"]) || truthy(&a["storageKey"]))) {
        let path = text(a, "path");
        let k = key_of(a, &path);
        if !k.is_empty() && seen.contains(&k) {
            // The same file as the main one (the API lists it twice): its name is the better one
            let mut name = file_name(a);
            if name.is_empty() {
                name = basename(&path).to_string();
            }
            let low = name.to_lowercase();
            if !name.is_empty() && !low.starts_with("cover") && !low.starts_with("preview") {
                if let Some(first) = files.first_mut().filter(|m| m["is_main"] == json!(true)) {
                    first.insert("name".into(), json!(name));
                }
            }
            continue;
        }
        if !k.is_empty() {
            seen.insert(k);
        }
        let mut name = file_name(a);
        if name.is_empty() {
            name = basename(if path.is_empty() { "attachment" } else { &path }).to_string();
        }
        let th = file_thumb(a, &path, &name, domain);
        let video = if th.is_empty() { video_url(a, &path, &name, domain) } else { String::new() };
        files.push(entry(a, name, path, th, video, false));
    }

    // The post's text
    let raw = ["content", "captionHtml", "caption"].iter().map(|k| text(p, k)).find(|t| !t.is_empty()).unwrap_or_default();
    let c = re!(r"(?i)<br\s*/?>").replace_all(&raw, "\n");
    let c = re!(r"<[^>]+>").replace_all(&c, "");
    let content = html_unescape(c.trim());

    let n = files.len();
    let files: Vec<Value> = files.into_iter().enumerate().map(|(i, mut m)| {
        m.insert("postedIndex".into(), json!(i));
        Value::Object(m)
    }).collect();
    json!({"id": pid, "title": title, "content": content, "published": published, "thumbnail": thumb_url,
           "fileCount": n, "files": files, "selected": true})
}

pub fn cards(posts: &[Value], domain: &str, stop: &dyn Fn() -> bool) -> Vec<Value> {
    let domain = if domain.is_empty() { "pawchive.pw" } else { domain };
    let mut out = Vec::with_capacity(posts.len());
    for p in posts {
        if stop() {
            break;
        }
        out.push(card(p, domain));
    }
    out
}

fn norm_path(v: &Value) -> String {
    if !truthy(v) {
        return String::new();
    }
    let s = pystr(v).replace('\\', "/");
    let s = s.trim();
    let s = s.strip_prefix("/data").filter(|r| r.starts_with('/')).unwrap_or(s);
    let s = if s.starts_with('/') { s.to_string() } else { format!("/{s}") };
    s.to_lowercase()
}

/// The posts ticked, in scan order; where some of a post's files were picked ({post id: [file
/// paths]}), only those
pub fn picked(posts: &[Value], ids: &[String], files: &Map<String, Value>) -> Vec<Value> {
    let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for p in posts {
        let pid = p.get("id").map(pystr).unwrap_or_default();
        if !wanted.contains(pid.as_str()) {
            continue;
        }
        let Some(allowed) = files.get(&pid).and_then(Value::as_array) else {
            out.push(p.clone());
            continue;
        };
        let exact: HashSet<String> = allowed.iter().map(pystr).collect();
        let norm: HashSet<String> = allowed.iter().map(norm_path).collect();
        let names: HashSet<String> = allowed.iter().filter(|x| truthy(x)).map(|x| basename(&pystr(x)).to_lowercase()).collect();
        let keep = |f: &Value| {
            if !f.is_object() {
                return false;
            }
            let fp = ["path", "storageKey"].iter().map(|k| text(f, k)).find(|t| !t.is_empty()).unwrap_or_default();
            let mut name = text(f, "name");
            if name.is_empty() {
                name = basename(&fp).to_string();
            }
            let name = name.to_lowercase();
            exact.contains(&fp) || norm.contains(&norm_path(&json!(fp))) || (!name.is_empty() && names.contains(&name))
        };
        let mut c = p.clone();
        if let Some(o) = c.as_object_mut() {
            if let Some(a) = p.get("attachments").and_then(Value::as_array) {
                o.insert("attachments".into(), Value::Array(a.iter().filter(|x| keep(x)).cloned().collect()));
            }
            if let Some(f) = p.get("file").filter(|f| f.is_object()) {
                if !keep(f) {
                    o.insert("file".into(), json!({}));
                }
            }
        }
        out.push(c);
    }
    out
}

/// The last picker scan: its link, creator and posts
struct Scanned {
    link: ParsedLink,
    creator_name: String,
    posts: Vec<Value>,
}

fn last() -> &'static Mutex<Option<Scanned>> {
    static L: OnceLock<Mutex<Option<Scanned>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(None))
}

pub fn scan(id: u64, a: &Value) -> Result<Value, String> {
    let c = service::site_client(&a["site"]);
    let cancel = service::cancel_flag(id);
    let cancelled = || cancel.load(Ordering::Relaxed);
    let p = service::parsed_link(&a["link"]);
    let n = |k: &str, d: usize| a[k].as_u64().map(|x| x as usize).unwrap_or(d);
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let profile = c.creator_profile(&p);
    let raw = profile.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(&p.user_id).to_string();
    let name = { let t = clean_text(&raw); if t.is_empty() { p.user_id.clone() } else { t } };
    let (page_start, page_end) = (n("page_start", 1), n("page_end", 999_999));
    let progress = |page: usize, count: usize, total: Option<u64>| service::progress_pages(id, page, count, total);
    let posts: Vec<Value> = if p.is_single_post {
        c.single_post(&p).into_iter().collect()
    } else if p.is_dms {
        c.user_dms(&p, page_start, page_end, 50, &progress, &cancel)
    } else {
        let mut v = c.user_posts(&p, page_start, page_end, 50, &progress, &cancel, &s("date_after"), &s("date_before"));
        if a["dms"].as_bool().unwrap_or(false) {
            v.extend(c.user_dms(&p, page_start, page_end, 50, &progress, &cancel));
        }
        v
    };
    if cancelled() {
        return Ok(json!({"status": "cancelled", "creator_name": name}));
    }
    if posts.is_empty() {
        return Ok(json!({"status": "no_posts", "creator_name": name}));
    }
    let list = cards(&posts, &p.domain, &cancelled);
    *last().lock().unwrap() = Some(Scanned { link: p, creator_name: name.clone(), posts });
    Ok(json!({"status": "cards", "creator_name": name, "cards": list}))
}

pub fn plan(id: u64, a: &Value) -> Result<Value, String> {
    let ids: Vec<String> = a["ids"].as_array().into_iter().flatten().map(pystr).collect();
    let files = a["files"].as_object().cloned().unwrap_or_default();
    let (posts, link, name) = {
        let g = last().lock().unwrap();
        let Some(sc) = g.as_ref() else { return Ok(json!({"status": "gone"})) };
        (picked(&sc.posts, &ids, &files), sc.link.clone(), sc.creator_name.clone())
    };
    if posts.is_empty() {
        return Ok(json!({"status": "empty"}));
    }
    let args = json!({
        "options": a["options"], "archive": a["archive"], "archive_db": a["archive_db"], "cookie": a["cookie"],
        "request": {"creator_name": name, "service": link.service, "domain": link.domain, "base_dir": a["base_dir"],
                    "batch_id": format!("{}_{}", link.service, link.user_id), "artist_dir": "", "user_id": "",
                    "include_service": a["include_service"].as_bool().unwrap_or(true)},
    });
    let cancel = service::cancel_flag(id);
    let plan = service::plan_build(id, &args, &posts, Some(cancel));
    if plan.cancelled {
        return Ok(json!({"status": "cancelled"}));
    }
    Ok(json!({"status": "plan", "posts": posts.len(), "creator_name": name, "plan": super::scan::plan_value(&plan)}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_picker() {
        let f: Value = serde_json::from_str(include_str!("../../tests/fixtures/selection.json")).unwrap();
        let posts = f["posts"].as_array().unwrap();
        for set in f["cards"].as_array().unwrap() {
            let domain = set[0].as_str().unwrap();
            let got = cards(posts, domain, &|| false);
            for (g, want) in got.iter().zip(set[1].as_array().unwrap()) {
                assert_eq!(g, want, "card on {domain:?}");
            }
            assert_eq!(got.len(), set[1].as_array().unwrap().len());
        }
        for pick in f["picks"].as_array().unwrap() {
            let ids: Vec<String> = pick[0].as_array().unwrap().iter().map(pystr).collect();
            let got = picked(posts, &ids, pick[1].as_object().unwrap());
            assert_eq!(Value::Array(got), pick[2], "pick {:?}", pick[0]);
        }
    }
}
