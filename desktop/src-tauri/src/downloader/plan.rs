//! Turning a creator's posts into the list of files to download: which posts and files pass the
//! filters, the folder and file name of each, the servers to try, what's already on disk or in
//! the download archive, embedded videos for yt-dlp, post_info.txt texts, passwords, and the
//! links-only scan. A port of FileDownloader.build_tasks_from_posts (core/downloader.py).
//!
//! The download archive, video length probing and Pawchive's
//! temporary links come from the caller (`Lookups`), so the planner itself is plain logic.

use super::extract;
use super::filters::{self as f, truthy, Options, AUDIO_EXTS, IMAGE_EXTS, VIDEO_EXTS};
use super::naming::{self as n, NameParts};
use super::order::order_files;
use super::py::{basename, dirname, join, pystr, splitext};
use md5::{Digest, Md5};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

const POST_INFO_NAME: &str = "post_info.txt";
const ARCHIVE_EXTENSIONS: &[&str] = &[".7z", ".zip", ".rar", ".tar", ".gz", ".bz2", ".xz", ".tgz", ".tbz2", ".txz", ".zst", ".iso", ".cab", ".lzma"];

/// What the planner asks the rest of the app
pub trait Lookups {
    fn archive_enabled(&self) -> bool {
        false
    }
    fn is_archived(&self, _service: &str, _post_id: &str, _file_id: &str, _sha256: &str) -> bool {
        false
    }
    #[allow(clippy::too_many_arguments)]
    fn record_link(&self, _service: &str, _creator_id: &str, _post_id: &str, _url: &str, _creator_name: &str, _post_title: &str, _link_title: &str) {}
    /// Fill in the signed links of a Pawchive post's oversized files
    fn resolve_temporary_links(&self, _post: &mut Value, _domain: &str) {}
    fn record_archive_password(&self, _archive_path: &str, _password: &str) {}
    fn add_passwords(&self, _passwords: &[String]) {}
    /// The user cancelled while planning
    fn cancelled(&self) -> bool {
        false
    }
    /// A line for the log panel (to_file: also in the log file)
    fn log(&self, _level: &str, _message: &str, _to_file: bool) {}
}

pub struct NoLookups;
impl Lookups for NoLookups {}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Task {
    pub url: String,
    pub target_path: String,
    pub post_title: String,
    pub creator_name: String,
    pub service: String,
    pub post_id: String,
    pub file_id: String,
    pub file_size: i64,
    pub expected_sha256: String,
    pub is_ytdlp: bool,
    pub batch_id: String,
    pub post_url: String,
    pub post_date: String,
    pub user_id: String,
    pub fallback_urls: Vec<String>,
    /// post_info.txt to write once this post's first file is saved
    pub post_info_path: String,
    #[serde(default)]
    pub is_telegram: bool,
    /// A Telegram file's channel and message (kept for the crash journal)
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub telegram_channel_id: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub telegram_message_id: Value,
    /// The queue's name for this file (the app's own task object)
    #[serde(default)]
    pub key: String,
}

impl Task {
    fn batch_or_default(batch_id: Option<&str>, service: &str, creator: &str, post_id: &str) -> String {
        match batch_id.filter(|b| !b.is_empty()) {
            Some(b) => b.to_string(),
            None if !service.is_empty() || !creator.is_empty() || !post_id.is_empty() => {
                format!("{service}_{creator}_{post_id}").trim_matches('_').to_string()
            }
            None => "batch_default".into(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub tasks: Vec<Task>,
    /// (post_info path, post id, text): written once a file of the post is saved
    pub pending_post_infos: Vec<(String, String, String)>,
    /// (post_info path, post id, text): to write now (text-only posts, posts with nothing new)
    pub write_now: Vec<(String, String, String)>,
    /// Links-only scan: {platform: links}, and one record per link
    pub harvested_links: BTreeMap<String, Vec<String>>,
    pub harvested_records: Vec<Value>,
    /// Why files weren't queued / were renamed: {"already on disk": 12, ...}
    pub not_queued: BTreeMap<String, usize>,
    pub renamed: BTreeMap<String, usize>,
    pub cancelled: bool,
}

pub struct Request<'a> {
    pub creator_name: &'a str,
    pub service: &'a str,
    pub domain: &'a str,
    pub base_dir: &'a str,
    pub batch_id: Option<&'a str>,
    pub artist_dir: Option<&'a str>,
    pub user_id: &'a str,
    /// "Site in Folder Name": "Artist [service]" instead of "Artist"
    pub include_service: bool,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn text<'a>(o: &'a Value, k: &str) -> &'a str {
    o.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The first of the keys whose value is truthy, as text
fn first_text(o: &Value, keys: &[&str]) -> String {
    keys.iter().find_map(|k| o.get(*k).filter(|v| truthy(v)).map(pystr)).unwrap_or_default()
}

fn flag(o: &Value, k: &str) -> bool {
    o.get(k).is_some_and(truthy)
}

/// Thumbnail-server URLs: a small preview copy, never the full-size original
pub fn is_preview_url(url: &str) -> bool {
    let u = url.to_lowercase();
    !u.is_empty() && (u.contains("/thumbnail/") || u.contains("img.pawchive.pw") || u.contains("img.cum.st"))
}

/// The file's identity across mirrors: its /ab/cd/... path, or a cum.st storage key
pub fn norm_rel_key(f: &Value) -> String {
    if !f.is_object() {
        return String::new();
    }
    let rp = first_text(f, &["path", "storageKey"]);
    if rp.is_empty() {
        return String::new();
    }
    let rp = rp.split('?').next().unwrap_or("").to_string();
    if re!(r"^[0-9a-f]{16,}$").is_match(&rp) {
        return rp.to_lowercase();
    }
    if let Some(c) = re!(r"(?i)/(?:data|thumbnail/data)?(/[0-9a-f]{2}/[0-9a-f]{2}/[^\s?#]+)").captures(&rp) {
        return c[1].to_lowercase();
    }
    let mut rp = rp;
    if rp.contains("://") {
        rp = format!("/{}", rp.rsplit("://").next().unwrap_or("").split_once('/').map(|x| x.1).unwrap_or(""));
    }
    if let Some(r) = rp.strip_prefix("/data/") {
        rp = format!("/{r}");
    }
    if !rp.starts_with('/') {
        rp = format!("/{rp}");
    }
    rp.to_lowercase()
}


/// Other names the same media may have on disk (.webp/.avif for pictures, .mp4/.mkv/.webm for videos)
pub fn media_candidate_paths(path: &str) -> Vec<String> {
    if path.is_empty() {
        return Vec::new();
    }
    let (stem, ext) = splitext(path);
    let low = ext.to_lowercase();
    let mut out = vec![path.to_string()];
    if VIDEO_EXTS.contains(&low.as_str()) {
        out.extend([".mp4", ".mkv", ".webm"].iter().filter(|e| **e != low).map(|e| format!("{stem}{e}")));
    } else if IMAGE_EXTS.contains(&low.as_str()) || low == ".webp" || low == ".avif" {
        out.extend([".webp", ".avif"].iter().filter(|e| **e != low).map(|e| format!("{stem}{e}")));
    }
    out
}

/// An existing file is a small / preview copy that the full-size one should replace
pub fn needs_full_size_upgrade(existing_size: i64, server_size: i64, target_path: &str, o: &Options, existing_path: &str) -> bool {
    if o.flag("download_thumbnails_only") {
        return false;
    }
    let target_ext = splitext(&target_path.to_lowercase()).1.to_string();
    let exist_ext = splitext(&existing_path.to_lowercase()).1.to_string();
    if VIDEO_EXTS.contains(&target_ext.as_str()) || VIDEO_EXTS.contains(&exist_ext.as_str()) {
        return false;
    }
    let lossy = |e: &str| e == ".webp" || e == ".avif";
    if lossy(&exist_ext) && !lossy(&target_ext) {
        return false;
    }
    if server_size > 0 {
        return (existing_size as f64) < server_size as f64 * 0.75;
    }
    if o.flag("redownload_small_files") {
        let threshold = f::effective_size_limits(o).0.filter(|m| *m != 0).unwrap_or(150 * 1024);
        return existing_size < threshold && (IMAGE_EXTS.contains(&target_ext.as_str()) || lossy(&target_ext));
    }
    false
}

/// The post id as it goes into a file name (album "posts" use a link as their id)
pub fn post_id_tag(post_id: &str) -> String {
    let n = post_id.chars().count();
    if (1..=40).contains(&n) && post_id.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-') {
        return post_id.to_string();
    }
    hex6(&Md5::digest(post_id.as_bytes()), 8)
}

fn hex6(bytes: &[u8], len: usize) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()[..len].to_string()
}

/// A file name for the ?f= link parameter
fn fq(name: &str) -> String {
    urlencoding::encode(name).into_owned()
}

fn mime_ext(mime: &str) -> &'static str {
    match mime.to_lowercase().split(';').next().unwrap_or("").trim() {
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/avif" => ".avif",
        "video/mp4" => ".mp4",
        "video/webm" => ".webm",
        "video/quicktime" => ".mov",
        "video/x-matroska" => ".mkv",
        "video/avi" => ".avi",
        "audio/mpeg" => ".mp3",
        "audio/ogg" => ".ogg",
        "audio/wav" => ".wav",
        "audio/flac" => ".flac",
        "audio/aac" => ".aac",
        _ => "",
    }
}

fn is_disabled(url: &str) -> bool {
    matches!(super::links::archive_site(url), Some("kemono" | "coomer"))
}

fn nonempty_file(p: &str) -> Option<i64> {
    std::fs::metadata(p).ok().filter(|m| m.is_file() && m.len() > 0).map(|m| m.len() as i64)
}

fn post_date(post: &Value) -> String {
    match post.get("published").filter(|v| truthy(v)) {
        Some(Value::Number(num)) => {
            let secs = num.as_f64().unwrap_or(0.0);
            chrono::DateTime::from_timestamp(secs.floor() as i64, 0)
                .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| pystr(&Value::Number(num.clone())))
        }
        Some(v) => {
            let s = pystr(v);
            if s.contains('T') { s.split('T').next().unwrap_or("").to_string() } else { super::py::take_chars(&s, 10) }
        }
        None => String::new(),
    }
}

/// A cover / preview picture by its name
fn looks_like_cover(name: &str, path: &str, with_thumbs: bool) -> bool {
    let starts: &[&str] = if with_thumbs { &["cover.", "cover_", "preview.", "thumb.", "thumbnail."] } else { &["cover.", "cover_", "preview."] };
    starts.iter().any(|s| name.starts_with(s)) || (with_thumbs && (is_preview_url(path) || re!(r"(_low|_thumb|\.thumb)\.[a-z0-9]+$").is_match(name)))
}

fn bump(c: &mut BTreeMap<String, usize>, why: &str) {
    *c.entry(why.to_string()).or_insert(0) += 1;
}

// ── The planner ─────────────────────────────────────────────────────────────

pub fn build(posts: &[Value], o: &Options, r: &Request, ctx: &dyn Lookups) -> Plan {
    let mut plan = Plan::default();
    let creator_clean = n::clean_filesystem_text(r.creator_name, 80, "creator");
    let (service, domain) = (r.service, r.domain);

    // Manga mode: oldest first. Tag folders: by first tag, then date.
    let mut posts: Vec<Value> = posts.to_vec();

    let mut extracted_links: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut extracted_records: Vec<Value> = Vec::new();
    let mut folder_counts: HashMap<String, i64> = HashMap::new();
    let mut post_folders: HashMap<String, String> = HashMap::new();
    let mut batch_paths: HashMap<String, (String, String)> = HashMap::new(); // path -> (post id, rel path)
    let mut batch_rel_paths: HashMap<String, HashSet<String>> = HashMap::new();
    let mut text_posts: Vec<(String, String, String)> = Vec::new();
    let mut creator_folder_name: Option<String> = None;
    let file_type = o.str("file_type");
    let group = o.str("group_file_type");
    let allow_thumbnails = o.flag("download_thumbnails_only") || o.flag("fallback_to_thumbnails");

    for (post_idx, post) in posts.iter_mut().enumerate() {
        let post_idx = post_idx as i64 + 1;
        if ctx.cancelled() {
            plan.cancelled = true;
            plan.tasks.clear();
            return plan;
        }
        let post_id = post.get("id").map(pystr).unwrap_or_default();
        let (post_title, post_caption) = n::post_title_and_caption(post, 80, "Untitled");
        let date_str = post_date(post);
        let post_user = post.get("user").filter(|v| truthy(v)).map(pystr).unwrap_or_default();
        let task_post_url = match (!domain.is_empty() && !service.is_empty(), !post_user.is_empty(), !post_id.is_empty()) {
            (true, true, true) => format!("https://{domain}/{service}/user/{post_user}/post/{post_id}"),
            (true, false, true) => format!("https://{domain}/{service}/post/{post_id}"),
            _ => String::new(),
        };

        if !f::should_keep_post(post, o).0 {
            continue;
        }

        // Links-only scan: collect the post's external links, download nothing
        if file_type == "links" {
            for (platform, urls) in extract::links_from_post(post) {
                for u in &urls {
                    extracted_records.push(json!({
                        "title": post_title, "url": u, "platform": platform, "service": service, "creator": r.creator_name,
                        "post_id": post_id, "tags": post.get("tags").filter(|t| truthy(t)).cloned().unwrap_or(json!([])),
                    }));
                }
                extracted_links.entry(platform).or_default().extend(urls);
            }
            continue;
        }

        // The post's folder
        let creator_folder_default = creator_folder_name
            .get_or_insert_with(|| {
                let p = n::pick_creator_folder(Path::new(r.base_dir), &creator_clean, service, r.include_service);
                basename(&p.to_string_lossy()).to_string()
            })
            .clone();
        let creator_folder = creator_folder_default;
        let mut parts: Vec<String> = if let Some(a) = r.artist_dir.filter(|a| !a.is_empty()) {
            vec![a.to_string()]
        } else {
            vec![r.base_dir.to_string(), creator_folder.clone()]
        };

        let creator_base = parts.clone();
        let mut post_subfolder = String::new();
        if o.flag("subfolder_per_post") {
            let clean_title = n::clean_filesystem_text(&post_title, 100, "Untitled");
            let mut folder = if o.flag("date_prefix") && !date_str.is_empty() { format!("[{date_str}] {clean_title}") } else { clean_title };
            let joined = |parts: &[String], last: &str| join(&parts.iter().map(String::as_str).chain([last]).collect::<Vec<_>>());
            let mut cand = joined(&parts, &folder);
            if post_folders.get(&cand).is_some_and(|other| *other != post_id) {
                folder = format!("{folder} [{post_id}]");
                cand = joined(&parts, &folder);
            }
            post_folders.insert(cand, post_id.clone());
            parts.push(folder.clone());
            post_subfolder = folder;
        }
        fn refs(v: &[String]) -> Vec<&str> {
            v.iter().map(String::as_str).collect()
        }
        let post_folder = join(&refs(&parts));
        let dest_folder = |name: &str| -> String {
            if group == "post" || group == "creator" {
                let cat = f::file_type_category(name);
                if group == "creator" {
                    let mut p = refs(&creator_base);
                    p.push(cat);
                    if o.flag("subfolder_per_post") && !post_subfolder.is_empty() {
                        p.push(&post_subfolder);
                    }
                    return join(&p);
                }
                return join(&[&post_folder, cat]);
            }
            post_folder.clone()
        };

        // Passwords: the post's text and comments
        let caption_text = {
            let t = first_text(post, &["captionText", "content", "captionHtml", "caption", "text"]);
            if t.is_empty() { post_caption.clone() } else { t }
        };
        let caption_clean = if !post_caption.is_empty() {
            post_caption.clone()
        } else {
            let t = re!(r"(?i)<br\s*/?>").replace_all(&caption_text, "\n");
            re!(r"<[^>]+>").replace_all(&t, "").trim().to_string()
        };
        let mut pw_text = caption_clean.clone();
        let comments_text = post.get("comments_text").filter(|v| truthy(v)).map(pystr);
        if let Some(c) = &comments_text {
            pw_text.push('\n');
            pw_text.push_str(c);
        }
        let passwords = if pw_text.is_empty() { Vec::new() } else { extract::passwords(&pw_text) };

        // post_info.txt: prepared now, written once a file of the post is saved
        let task_start = plan.tasks.len();
        let mut post_info: Option<(String, String)> = None;
        if o.flag("save_post_metadata") {
            let info_dir = if post_subfolder.is_empty() || group == "creator" { dest_folder(POST_INFO_NAME) } else { post_folder.clone() };
            let info_path = n::fit_path_for_windows(&join(&[&info_dir, POST_INFO_NAME]), n::WINDOWS_MAX_PATH);
            let prepare = if !post_subfolder.is_empty() {
                !Path::new(&info_path).exists()
            } else if Path::new(&info_path).exists() {
                match std::fs::read(&info_path) {
                    Ok(b) => !String::from_utf8_lossy(&b).contains(&format!("Post ID: {post_id}")),
                    Err(_) => true,
                }
            } else {
                true
            };
            if prepare {
                post_info = Some((info_path, post_info_text(post, &post_title, &post_id, &date_str, &task_post_url, &passwords, &caption_clean, r)));
            }
        }

        // External links and embeds go into the download archive
        if ctx.archive_enabled() {
            let creator_id = if r.user_id.is_empty() { post_user.as_str() } else { r.user_id };
            for (plat, urls) in extract::links_from_post(post) {
                for u in urls {
                    ctx.record_link(service, creator_id, &post_id, &u, r.creator_name, &post_title, &format!("[{}] {u}", plat.to_uppercase()));
                }
            }
            for e in extract::embed_urls(post) {
                ctx.record_link(service, creator_id, &post_id, &e, r.creator_name, &post_title, &e);
            }
        }

        // The post's files: the main file and attachments, without duplicates
        if o.flag("download_pawchive_temporary_files") {
            let any_pawchive = domain.contains("pawchive") || ["origin", "domain", "service"].iter().any(|k| post.get(*k).map(pystr).unwrap_or_default().contains("pawchive"));
            if any_pawchive {
                ctx.resolve_temporary_links(post, if domain.is_empty() { "pawchive.pw" } else { domain });
            }
        }
        let main_file = post.get("file").filter(|m| m.as_object().is_some_and(|o| !o.is_empty())).cloned();
        let attachments: Vec<Value> = match post.get("attachments") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let has_path = |x: &Value| !first_text(x, &["path", "storageKey"]).is_empty();
        let has_valid_attachments = attachments.iter().any(|a| a.is_object() && has_path(a) && !flag(a, "locked"));
        let content_html = first_text(post, &["content", "captionHtml"]);
        let has_content_images = o.flag("scan_content_images") && !n::content_images(&content_html).is_empty();
        let has_other = has_valid_attachments || has_content_images;

        let mut skip_cover = false;
        if let Some(m) = &main_file {
            let name = first_text(m, &["name", "originalFilename"]).to_lowercase();
            if o.flag("skip_post_covers") {
                skip_cover = has_other || looks_like_cover(&name, "", false);
            } else if has_other && !allow_thumbnails {
                let path = first_text(m, &["path", "storageKey"]).to_lowercase();
                let temp = flag(m, "is_temporary") || path.contains("t1.pawchive.pw");
                skip_cover = !temp && (looks_like_cover(&name, &path, true) || flag(m, "preview_only"));
            }
        } else if o.flag("skip_post_covers") && has_other {
            skip_cover = true;
        }

        let mut files: Vec<Value> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        if let Some(m) = main_file.as_ref().filter(|m| !skip_cover && has_path(m)) {
            let k = norm_rel_key(m);
            if !k.is_empty() {
                seen.insert(k, 0);
            }
            files.push(m.clone());
        }
        let drop_covers = (o.flag("skip_post_covers") || (has_other && !allow_thumbnails)) && (has_other || attachments.len() > 1);
        for att in attachments.iter().filter(|a| a.is_object() && has_path(a)) {
            if drop_covers {
                let name = first_text(att, &["name", "originalFilename"]).to_lowercase();
                let path = first_text(att, &["path", "storageKey"]).to_lowercase();
                let temp = flag(att, "is_temporary") || path.contains("t1.pawchive.pw");
                if !temp && looks_like_cover(&name, &path, true) {
                    continue;
                }
            }
            let k = norm_rel_key(att);
            if let Some(&idx) = seen.get(&k).filter(|_| !k.is_empty()) {
                let existing = files[idx].as_object_mut().unwrap();
                if existing.get("preview_only").is_some_and(truthy) && !flag(att, "preview_only") {
                    existing.insert("preview_only".into(), json!(false));
                }
                if let Some(nm) = att.get("name").filter(|v| truthy(v)) {
                    if !text(att, "name").to_lowercase().starts_with("cover") {
                        existing.insert("name".into(), nm.clone());
                    }
                }
                continue;
            }
            if !k.is_empty() {
                seen.insert(k, files.len());
            }
            files.push(att.clone());
        }
        let post_file_count = files.len();

        // Pictures embedded in the post's text
        if o.flag("scan_content_images") {
            let exact = o.str("exact_extensions");
            let allowed = if exact.is_empty() { None } else { Some(f::parse_extensions_list(&exact)) };
            if allowed.as_ref().is_none_or(|a| a.iter().any(|e| IMAGE_EXTS.contains(&e.as_str()))) {
                for ci in n::content_images(&content_html) {
                    let d = json!({"name": basename(&ci), "path": ci});
                    let k = norm_rel_key(&d);
                    if !k.is_empty() {
                        if seen.contains_key(&k) {
                            continue;
                        }
                        seen.insert(k, files.len());
                    }
                    files.push(d);
                }
            }
        }

        // "File order in posts": the post's files and its embedded pictures are ordered separately
        let file_order = o.str("file_order");
        if file_order != "posted" && files.len() > 1 {
            let rest = files.split_off(post_file_count);
            let name_of = |x: &Value| first_text(x, &["name", "originalFilename"]);
            files = order_files(files, &file_order, name_of);
            files.extend(order_files(rest, &file_order, name_of));
        }

        let caption_for_names = caption_clean.clone();
        for (file_idx, fobj) in files.iter().enumerate() {
            let file_idx = file_idx as i64 + 1;
            if flag(fobj, "locked") {
                continue;
            }
            let mut raw_name = super::py::rstrip_chars(first_text(fobj, &["name", "originalFilename"]).trim(), ".,;!?").to_string();
            let m_ext = mime_ext(&first_text(fobj, &["mimeType", "mime_type"]));
            if raw_name.is_empty() || splitext(&raw_name).1.is_empty() {
                let mut base_id = first_text(fobj, &["storageKey", "sha256"]);
                if base_id.is_empty() {
                    base_id = fobj.get("id").filter(|v| truthy(v)).map(pystr).unwrap_or_else(|| format!("file_{file_idx}"));
                }
                raw_name = if raw_name.is_empty() { format!("{base_id}{}", if m_ext.is_empty() { ".jpg" } else { m_ext }) } else { format!("{raw_name}{m_ext}") };
            }
            let rel_path = first_text(fobj, &["path", "storageKey"]);
            if rel_path.is_empty() {
                continue;
            }
            let file_bytes = fobj.get("bytes").and_then(|b| b.as_i64().or_else(|| b.as_f64().map(|x| x as i64)));
            if !f::should_keep_file(&raw_name, o, file_bytes).0 {
                bump(&mut plan.not_queued, "filtered out");
                continue;
            }

            // Where the file is: its path on the site, and its full link when the site gave one
            let mut original_url: Option<String> = None;
            let mut norm_path = rel_path.clone();
            let is_storage_key = re!(r"^[0-9a-f]+$").is_match(&norm_path) && !norm_path.starts_with("http") && !norm_path.contains('/') && norm_path.len() >= 16;
            let mut extra_variants: Vec<String> = Vec::new();
            if is_storage_key && domain.contains("cum.st") {
                let mut primary: Option<String> = None;
                if let Some(vs) = fobj.get("variants").and_then(Value::as_array).filter(|v| !v.is_empty()) {
                    for v in vs {
                        if let Some(vn) = v.get("name").filter(|x| truthy(x)).map(pystr) {
                            if vn.to_lowercase().contains("original") {
                                primary = Some(vn);
                            } else {
                                extra_variants.push(vn);
                            }
                        }
                    }
                    if primary.is_none() && vs[0].is_object() {
                        primary = vs[0].get("name").filter(|x| truthy(x)).map(pystr);
                    }
                }
                let fn_ext = splitext(&raw_name).1.trim_start_matches('.').to_string();
                let ext = if !fn_ext.is_empty() { fn_ext } else if !m_ext.is_empty() { m_ext.trim_start_matches('.').to_string() } else { "jpg".into() };
                let primary = primary.unwrap_or_else(|| format!("original.{ext}"));
                original_url = Some(format!("https://cdn.cum.st/media/{norm_path}/{primary}"));
                norm_path = format!("/{}/{}/{norm_path}.{ext}", &norm_path[..2], &norm_path[2..4]);
            } else if norm_path.starts_with("http://") || norm_path.starts_with("https://") {
                let after_host = |u: &str| u.rsplit("://").next().unwrap_or("").split_once('/').map(|x| x.1.to_string()).unwrap_or_default();
                if norm_path.contains("t1.pawchive.pw") || flag(fobj, "is_temporary") {
                    // Pawchive's signed temporary links keep their query
                    original_url = Some(norm_path.clone());
                    let clean = after_host(norm_path.split('?').next().unwrap_or(""));
                    norm_path = if clean.starts_with('/') { clean } else { format!("/{clean}") };
                } else {
                    original_url = Some(norm_path.split('?').next().unwrap_or("").replace("e1.cum.st", "cdn.cum.st"));
                    if let Some(c) = re!(r"^https?://[^/]*cum\.st/media/([0-9a-f]{64})/([^?#]+)").captures(&norm_path) {
                        let (key, variant) = (c[1].to_string(), c[2].to_string());
                        let e = splitext(&variant).1.trim_start_matches('.');
                        let e = if e.is_empty() { "jpg" } else { e };
                        norm_path = format!("/{}/{}/{key}.{e}", &key[..2], &key[2..4]);
                    } else if let Some(c) = re!(r"(?i)/(?:data|thumbnail/data)?(/[0-9a-f]{2}/[0-9a-f]{2}/[^\s?#]+)").captures(&norm_path) {
                        norm_path = c[1].to_string();
                    } else {
                        let p = after_host(&norm_path);
                        norm_path = if p.starts_with('/') { p } else { format!("/{p}") };
                    }
                }
            }
            let mut clean_rel = if norm_path.starts_with('/') { norm_path.clone() } else { format!("/{norm_path}") };
            if let Some(r) = clean_rel.strip_prefix("/data") {
                if r.starts_with('/') {
                    clean_rel = r.to_string();
                }
            }
            if !clean_rel.starts_with('/') {
                clean_rel = format!("/{clean_rel}");
            }

            // The same file twice in a folder
            let mut norm_key = norm_rel_key(fobj);
            if norm_key.is_empty() {
                norm_key = clean_rel.to_lowercase();
            }
            let raw_dest = dest_folder(&raw_name);
            if !o.flag("keep_duplicates") && batch_rel_paths.get(&raw_dest).is_some_and(|s| s.contains(&norm_key)) {
                bump(&mut plan.not_queued, "identical duplicates");
                continue;
            }
            let seq_idx = {
                let c = folder_counts.entry(raw_dest.clone()).or_insert(0);
                *c += 1;
                *c
            };

            let mut name = n::format_custom_filename(
                &NameParts {
                    original_filename: &raw_name,
                    post_title: &post_title,
                    post_date: &date_str,
                    post_index: post_idx,
                    file_index: file_idx,
                    folder_index: Some(seq_idx),
                    post_id: &post_id,
                    artist: r.creator_name,
                    service,
                    user_id: &post_user,
                    caption: &caption_for_names,
                },
                o,
            );
            name = super::py::rstrip_chars(&name, ".,;!? \t").to_string();

            let mut effective_domain = domain.to_string();
            if let Some(u) = &original_url {
                let host = u.rsplit("://").next().unwrap_or("").split('/').next().unwrap_or("").to_lowercase();
                if host.contains("cum.st") {
                    effective_domain = "cum.st".into();
                } else if host.contains("pawchive") {
                    effective_domain = "pawchive.pw".into();
                }
            }
            if !effective_domain.contains("cum.st") {
                effective_domain = "pawchive.pw".into();
            }
            let on_cumst = effective_domain.contains("cum.st");
            let where_from = original_url.clone().filter(|s| !s.is_empty()).or(Some(norm_path.clone()).filter(|s| !s.is_empty())).unwrap_or_else(|| text(fobj, "path").to_string());
            let is_preview_only = flag(fobj, "preview_only") && !flag(fobj, "is_temporary") && !where_from.contains("t1.pawchive.pw");
            if is_preview_only && !allow_thumbnails {
                bump(&mut plan.not_queued, "filtered out");
                continue;
            }

            // The servers to try, best first
            let mut cands: Vec<String> = Vec::new();
            let push = |c: &mut Vec<String>, u: String| {
                if !c.contains(&u) {
                    c.push(u);
                }
            };
            if is_preview_only || o.flag("download_thumbnails_only") {
                cands.push(format!("https://{}/thumbnail/data{clean_rel}", if on_cumst { "img.cum.st" } else { "img.pawchive.pw" }));
                // A video's thumbnail is a picture
                let lower = name.to_lowercase();
                if VIDEO_EXTS.contains(&splitext(&lower).1) {
                    name = format!("{}.jpg", splitext(&name).0);
                }
            } else {
                let is_temp = original_url.as_ref().is_some_and(|u| u.contains("t1.pawchive.pw")) || flag(fobj, "is_temporary");
                match original_url.as_ref().filter(|_| is_temp) {
                    Some(u) => cands.push(u.clone()),
                    None => {
                        let is_thumb_url = original_url.as_ref().is_some_and(|u| u.contains("/thumbnail/") || u.contains("img.pawchive") || u.contains("img.cum.st"));
                        if let Some(u) = original_url.as_ref().filter(|_| !is_thumb_url) {
                            cands.push(format!("{u}?f={}", fq(&name)));
                        }
                        let fallback_thumb;
                        if on_cumst {
                            if is_storage_key {
                                for ev in &extra_variants {
                                    push(&mut cands, format!("https://cdn.cum.st/media/{rel_path}/{ev}"));
                                }
                            } else if original_url.is_none() || is_thumb_url {
                                cands.push(format!("https://cum.st/data{clean_rel}?f={}", fq(&name)));
                            }
                            cands.push(format!("https://cum.st/data{clean_rel}"));
                            cands.push(format!("https://img.cum.st/data{clean_rel}"));
                            fallback_thumb = format!("https://img.cum.st/thumbnail/data{clean_rel}");
                        } else {
                            push(&mut cands, format!("https://file.pawchive.pw/data{clean_rel}?f={}", fq(&name)));
                            let node = fobj.get("node").filter(|v| truthy(v)).or_else(|| post.get("node")).filter(|v| !v.is_null()).cloned();
                            if let Some(nn) = node {
                                push(&mut cands, format!("https://n{}.pawchive.pw/data{clean_rel}?f={}", pystr(&nn), fq(&name)));
                            }
                            fallback_thumb = format!("https://img.pawchive.pw/thumbnail/data{clean_rel}");
                        }
                        if o.flag("fallback_to_thumbnails") {
                            push(&mut cands, fallback_thumb);
                            if let Some(u) = original_url.as_ref().filter(|u| is_thumb_url && !cands.contains(u)) {
                                cands.push(format!("{u}?f={}", fq(&name)));
                            }
                        }
                    }
                }
            }
            // Unsupported sites are never contacted
            cands.retain(|u| !is_disabled(u));
            let mut file_url = cands.first().cloned().unwrap_or_else(|| format!("https://file.pawchive.pw/data{clean_rel}?f={}", fq(&name)));
            let target_dir = dest_folder(&name);
            let mut target_path = n::fit_path_for_windows(&join(&[&target_dir, &name]), n::WINDOWS_MAX_PATH);
            let file_id = format!("{post_id}_{clean_rel}");

            // Two files after the same name: an identical one is dropped, others renamed
            if let Some((prev_post, prev_rel)) = batch_paths.get(&target_path).cloned() {
                if prev_post == post_id && prev_rel == clean_rel {
                    bump(&mut plan.not_queued, "identical duplicates");
                    continue;
                }
                let (stem, ext) = splitext(&name);
                let (stem, ext) = (stem.to_string(), ext.to_string());
                let fit = |nm: &str| n::fit_path_for_windows(&join(&[&target_dir, nm]), n::WINDOWS_MAX_PATH);
                let mut new_name;
                if prev_post != post_id {
                    let tag = post_id_tag(&post_id);
                    new_name = format!("{stem} [{tag}]{ext}");
                    target_path = fit(&new_name);
                    let mut counter = 2;
                    while batch_paths.contains_key(&target_path) {
                        new_name = format!("{stem} [{tag}] ({counter}){ext}");
                        target_path = fit(&new_name);
                        counter += 1;
                    }
                    bump(&mut plan.renamed, "name used by another post");
                } else {
                    let rel_stem = splitext(basename(&clean_rel)).0;
                    let chars: Vec<char> = rel_stem.chars().collect();
                    let mut hint: String = chars[chars.len().saturating_sub(6)..].iter().collect();
                    if hint.is_empty() {
                        hint = hex6(&Md5::digest(clean_rel.as_bytes()), 6);
                    }
                    new_name = format!("{stem}_{hint}{ext}");
                    target_path = fit(&new_name);
                    let mut counter = 2;
                    while batch_paths.contains_key(&target_path) {
                        new_name = format!("{stem}_{hint}_{counter}{ext}");
                        target_path = fit(&new_name);
                        counter += 1;
                    }
                    bump(&mut plan.renamed, "same name twice in a post");
                }
                let (old_q, new_q) = (fq(&name), fq(&new_name));
                for u in cands.iter_mut() {
                    *u = u.replace(&format!("?f={old_q}"), &format!("?f={new_q}")).replace(&format!("&f={old_q}"), &format!("&f={new_q}"));
                }
                if let Some(u) = cands.first() {
                    file_url = u.clone();
                }
            }
            batch_paths.insert(target_path.clone(), (post_id.clone(), clean_rel.clone()));
            batch_rel_paths.entry(target_dir.clone()).or_default().insert(norm_key);

            // Already downloaded: on disk under any of its names, or in the download archive
            let raw_dir = dest_folder(&raw_name);
            let mut disk: Vec<String> = Vec::new();
            for base in [target_path.clone(), join(&[&raw_dir, &raw_name]), join(&[&raw_dir, &format!("{seq_idx:03}_{raw_name}")])] {
                for c in media_candidate_paths(&base) {
                    if !disk.contains(&c) {
                        disk.push(c);
                    }
                }
            }
            let expected_sha = first_text(fobj, &["sha256", "hash"]);
            let server_size = file_bytes.unwrap_or(0);
            let on_disk = || disk.iter().find_map(|c| nonempty_file(c).map(|s| (c.clone(), s)));
            let shown = basename(&target_path).to_string();
            if ctx.archive_enabled() && ctx.is_archived(service, &post_id, &file_id, &expected_sha) {
                let upgrade = on_disk().is_some_and(|(p, s)| needs_full_size_upgrade(s, server_size, &target_path, o, &p));
                if !upgrade {
                    ctx.log("info", &format!("📦 Skipping archived file: '{shown}' (present in download archive)"), false);
                    bump(&mut plan.not_queued, "in the download archive");
                    continue;
                }
            }
            if !o.flag("keep_duplicates") {
                if let Some((p, s)) = on_disk() {
                    if !needs_full_size_upgrade(s, server_size, &target_path, o, &p) {
                        ctx.log("info", &format!("⏳ Skipping existing file: '{shown}' (already present on disk)"), false);
                        bump(&mut plan.not_queued, "already on disk");
                        continue;
                    }
                    ctx.log("info", &format!("🔄 Re-downloading '{shown}' ({}) to upgrade to full resolution", f::format_size_str(s)), true);
                }
            }

            let user_id = if r.user_id.is_empty() { post_user.clone() } else { r.user_id.to_string() };
            if let Some(pw) = passwords.first() {
                if ARCHIVE_EXTENSIONS.contains(&splitext(&target_path.to_lowercase()).1) {
                    ctx.record_archive_password(&target_path, pw);
                }
            }
            plan.tasks.push(Task {
                url: file_url,
                target_path,
                post_title: post_title.clone(),
                creator_name: r.creator_name.into(),
                service: service.into(),
                post_id: post_id.clone(),
                file_id,
                file_size: server_size,
                expected_sha256: expected_sha,
                is_ytdlp: false,
                batch_id: Task::batch_or_default(r.batch_id, service, r.creator_name, &post_id),
                post_url: task_post_url.clone(),
                post_date: date_str.clone(),
                user_id,
                fallback_urls: cands.get(1..).map(<[String]>::to_vec).unwrap_or_default(),
                post_info_path: String::new(),
                is_telegram: false,
                telegram_channel_id: Value::Null,
                telegram_message_id: Value::Null,
                key: String::new(),
            });
        }

        // Embedded videos (YouTube, Vimeo…) for yt-dlp
        let mut embeds_on = o.flag("download_embeds") && ["all", "videos", "audio"].contains(&file_type.as_str());
        if embeds_on && !o.str("exact_extensions").is_empty() {
            let allowed = f::parse_extensions_list(&o.str("exact_extensions"));
            if !allowed.is_empty() {
                embeds_on = allowed.iter().any(|e| VIDEO_EXTS.contains(&e.as_str()) || AUDIO_EXTS.contains(&e.as_str()));
            }
        }
        if embeds_on {
            for (i, e_url) in extract::embed_urls(post).into_iter().enumerate() {
                let i = i + 1;
                let host = e_url.rsplit("://").next().unwrap_or("").split('/').next().unwrap_or("");
                let host: String = host.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                let e_name = format!("embed_{i}_{host}.mp4");
                let e_path = n::fit_path_for_windows(&join(&[&dest_folder(&e_name), &e_name]), n::WINDOWS_MAX_PATH);
                let e_id = format!("embed_{post_id}_{i}");
                if ctx.archive_enabled() && ctx.is_archived(service, &post_id, &e_id, "") {
                    ctx.log("info", &format!("📦 Skipping archived embedded media: '{e_name}' (present in download archive)"), false);
                    bump(&mut plan.not_queued, "in the download archive");
                    continue;
                }
                if !o.flag("keep_duplicates") && nonempty_file(&e_path).is_some() {
                    continue;
                }
                plan.tasks.push(Task {
                    url: e_url,
                    target_path: e_path,
                    post_title: post_title.clone(),
                    creator_name: r.creator_name.into(),
                    service: service.into(),
                    post_id: post_id.clone(),
                    file_id: e_id,
                    is_ytdlp: true,
                    batch_id: Task::batch_or_default(r.batch_id, service, r.creator_name, &post_id),
                    post_url: task_post_url.clone(),
                    post_date: date_str.clone(),
                    user_id: if r.user_id.is_empty() { post_user.clone() } else { r.user_id.to_string() },
                    ..Default::default()
                });
            }
        }

        let new_tasks = plan.tasks.len() - task_start;
        if !passwords.is_empty() && new_tasks > 0 {
            ctx.add_passwords(&passwords);
        }
        if let Some((path, content)) = post_info {
            if new_tasks > 0 {
                match plan.pending_post_infos.iter_mut().find(|(p, id, _)| *p == path && *id == post_id) {
                    Some(e) => e.2 = content,
                    None => plan.pending_post_infos.push((path.clone(), post_id.clone(), content)),
                }
                for t in &mut plan.tasks[task_start..] {
                    t.post_info_path = path.clone();
                }
            } else if files.is_empty() {
                text_posts.push((path, post_id.clone(), content));
            } else if Path::new(&dirname(&path)).is_dir() {
                plan.write_now.push((path, post_id.clone(), content));
            }
        }
    }
    plan.write_now.extend(text_posts);

    if file_type == "links" {
        plan.harvested_links = extracted_links
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| {
                let mut v: Vec<String> = v.into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                v.sort();
                (k, v)
            })
            .collect();
        let mut seen = HashSet::new();
        plan.harvested_records = extracted_records.into_iter().filter(|r| seen.insert(r["url"].as_str().unwrap_or("").to_string())).collect();
        if ctx.archive_enabled() {
            for rec in &plan.harvested_records {
                let s = |k: &str| rec.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                ctx.record_link(&s("service"), r.user_id, &s("post_id"), &s("url"), &s("creator"), &s("title"), &s("title"));
            }
        }
    }
    plan
}

#[allow(clippy::too_many_arguments)]
fn post_info_text(post: &Value, title: &str, post_id: &str, date: &str, post_url: &str, passwords: &[String], caption: &str, r: &Request) -> String {
    let tags = n::normalize_tags(post.get("tags").unwrap_or(&Value::Null));
    let (domain, service) = (r.domain, r.service);
    let user = post.get("user").filter(|v| truthy(v)).map(pystr).unwrap_or_default();
    let creator_url = match (!domain.is_empty() && !service.is_empty(), user.is_empty()) {
        (true, false) => format!("https://{domain}/{service}/user/{user}"),
        (true, true) => format!("https://{domain}/{service}"),
        _ => String::new(),
    };

    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let has_path = |x: &Value| !first_text(x, &["path", "storageKey"]).is_empty();
    let clean_name = |x: &Value| super::py::rstrip_chars(text(x, "name").trim(), ".,;!?").to_string();
    if let Some(m) = post.get("file").filter(|m| m.is_object() && has_path(m)) {
        let k = norm_rel_key(m);
        if !k.is_empty() {
            seen.insert(k);
        }
        let nm = clean_name(m);
        if !nm.is_empty() {
            names.push(nm);
        }
    }
    for a in post.get("attachments").and_then(Value::as_array).into_iter().flatten().filter(|a| a.is_object() && has_path(a)) {
        let k = norm_rel_key(a);
        if !k.is_empty() && !seen.insert(k) {
            continue;
        }
        let nm = clean_name(a);
        if !nm.is_empty() {
            names.push(nm);
        }
    }
    let links: Vec<String> = extract::links_from_post(post).into_iter().flat_map(|(p, us)| us.into_iter().map(move |u| format!("[{}] {u}", p.to_uppercase()))).collect();
    let embeds = extract::embed_urls(post);

    let mut s = format!("Title: {title}\nPost ID: {post_id}\nCreator: {} [{service}]\n", r.creator_name);
    if !creator_url.is_empty() {
        s += &format!("Creator URL: {creator_url}\n");
    }
    if !post_url.is_empty() {
        s += &format!("Post URL: {post_url}\n");
    }
    s += &format!("Published: {date}\n");
    if !tags.is_empty() {
        s += &format!("Tags: {}\n", tags.join(", "));
    }
    if !passwords.is_empty() {
        s += "\n--- Detected Password(s) ---\n";
        for p in passwords {
            s += &format!("  {p}\n");
        }
    }
    s += &format!("\n--- Content ---\n{caption}\n");
    if let Some(c) = post.get("comments_text").filter(|v| truthy(v)) {
        s += &format!("\n--- Comments ---\n{}\n", pystr(c));
    }
    for (head, items) in [("Attached Files", &names), ("External Links", &links), ("Embedded Media", &embeds)] {
        if !items.is_empty() {
            s += &format!("\n--- {head} ---\n");
            for i in items.iter() {
                s += &format!("  {i}\n");
            }
        }
    }
    s
}

/// Saves a post_info.txt, or adds the post to the shared one (once)
pub fn write_post_info(path: &str, content: &str, post_id: &str) {
    let _ = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dirname(path))?;
        let p = Path::new(path);
        if !p.exists() {
            let tmp = format!("{path}.tmp");
            std::fs::write(&tmp, content)?;
            return std::fs::rename(&tmp, path);
        }
        let existing = std::fs::read(p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        if !post_id.is_empty() && existing.contains(&format!("Post ID: {post_id}")) {
            return Ok(());
        }
        let mut add = String::new();
        if !existing.is_empty() && !existing.ends_with("\n\n") {
            if !existing.ends_with('\n') {
                add.push('\n');
            }
            add += &format!("\n{}\n\n", "=".repeat(80));
        }
        add += content;
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(p)?.write_all(add.as_bytes())
    })();
}

/// The plan's tasks as the queue stores them (DownloadTask.to_dict)
pub fn task_dict(t: &Task, o: &Options) -> Value {
    let mut m: Map<String, Value> = serde_json::to_value(t).unwrap().as_object().cloned().unwrap();
    m.remove("post_info_path");
    m.remove("key");
    m.insert("filename".into(), json!(basename(&t.target_path)));
    for (k, v) in [("downloaded_bytes", json!(0)), ("status", json!("pending")), ("error_msg", json!("")), ("retry_count", json!(0)), ("is_telegram", json!(false)), ("telegram_channel_id", json!("")), ("telegram_message_id", json!(0)), ("used_preview", json!(false))] {
        m.insert(k.into(), v);
    }
    m.insert("options".into(), o.to_dict());
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "\\target\\plan_base");

    /// The stand-ins of tools/golden_plan.py
    struct Stub(HashSet<String>);
    impl Lookups for Stub {
        fn archive_enabled(&self) -> bool {
            !self.0.is_empty()
        }
        fn is_archived(&self, _: &str, _: &str, file_id: &str, _: &str) -> bool {
            self.0.contains(file_id)
        }
    }

    fn setup(files: &Value) {
        let _ = std::fs::remove_dir_all(BASE);
        std::fs::create_dir_all(BASE).unwrap();
        for spec in files.as_array().unwrap() {
            let spec = spec.as_str().unwrap();
            let (rel, content) = spec.split_once(':').unwrap_or((spec, ""));
            let p = join(&[BASE, &rel.replace('/', "\\")]);
            std::fs::create_dir_all(dirname(&p)).unwrap();
            std::fs::write(&p, if content.is_empty() { "x".repeat(100000) } else { content.to_string() }).unwrap();
        }
    }

    fn run(case: &Value) -> Value {
        setup(&case["files"]);
        let req = &case["request"];
        let s = |k: &str| req[k].as_str().unwrap_or("").to_string();
        let artist = req["artist_dir"].as_str().map(|a| a.replace("{BASE}", BASE));
        let (creator, service, domain, user, batch) = (s("creator"), s("service"), s("domain"), s("user_id"), req["batch_id"].as_str().map(str::to_string));
        let r = Request {
            creator_name: &creator,
            service: &service,
            domain: &domain,
            base_dir: BASE,
            batch_id: batch.as_deref(),
            artist_dir: artist.as_deref(),
            user_id: &user,
            include_service: req["include_service"].as_bool().unwrap_or(true),
        };
        let o = Options::from_dict(&case["options"]);
        let stub = Stub(case["archived"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect());
        let posts: Vec<Value> = case["posts"].as_array().unwrap().clone();
        let plan = build(&posts, &o, &r, &stub);
        let sub = |x: &str| x.replace(BASE, "{BASE}");
        let tasks: Vec<Value> = plan
            .tasks
            .iter()
            .map(|t| {
                let mut d = task_dict(t, &o);
                let m = d.as_object_mut().unwrap();
                m.remove("options");
                m.insert("post_info_path".into(), json!(t.post_info_path));
                for v in m.values_mut() {
                    match v {
                        Value::String(x) => *x = sub(x),
                        Value::Array(a) => a.iter_mut().for_each(|y| *y = json!(sub(y.as_str().unwrap()))),
                        _ => {}
                    }
                }
                d
            })
            .collect();
        let mut pending: Vec<Value> = plan.pending_post_infos.iter().map(|(p, id, c)| json!([[sub(p), id], c])).collect();
        pending.sort_by_key(|v| v.to_string());
        let mut records = plan.harvested_records.clone();
        records.sort_by_key(|r| r["url"].as_str().unwrap_or("").to_string());
        json!({
            "tasks": tasks,
            "pending_post_infos": pending,
            "writes": plan.write_now.iter().map(|(p, id, c)| json!([sub(p), id, c])).collect::<Vec<_>>(),
            "harvested_links": plan.harvested_links,
            "harvested_records": records,
        })
    }

    /// The first difference between two answers, as "where: python … rust …"
    fn diff(path: &str, want: &Value, got: &Value) -> Option<String> {
        match (want, got) {
            (Value::Object(a), Value::Object(b)) => {
                for k in a.keys().chain(b.keys()) {
                    if let Some(d) = diff(&format!("{path}.{k}"), a.get(k).unwrap_or(&Value::Null), b.get(k).unwrap_or(&Value::Null)) {
                        return Some(d);
                    }
                }
                None
            }
            (Value::Array(a), Value::Array(b)) => {
                for i in 0..a.len().max(b.len()) {
                    if let Some(d) = diff(&format!("{path}[{i}]"), a.get(i).unwrap_or(&Value::Null), b.get(i).unwrap_or(&Value::Null)) {
                        return Some(d);
                    }
                }
                None
            }
            (Value::Number(x), Value::Number(y)) if x.as_f64() == y.as_f64() => None,
            _ if want == got => None,
            _ => Some(format!("{path}\n    python: {}\n    rust:   {}", want.to_string().chars().take(300).collect::<String>(), got.to_string().chars().take(300).collect::<String>())),
        }
    }

    fn check(cases: &[Value]) {
        let mut wrong = Vec::new();
        for c in cases {
            if let Some(d) = diff("", &c["result"], &run(c)) {
                wrong.push(format!("{}: {d}", c["name"].as_str().unwrap()));
            }
        }
        let _ = std::fs::remove_dir_all(BASE);
        assert!(wrong.is_empty(), "{} of {} cases differ:\n{}", wrong.len(), cases.len(), wrong.join("\n"));
    }

    #[test]
    fn matches_python_planner() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/plan.json")).unwrap();
        check(&cases);
    }

    /// python tools/golden_plan.py --live, then: cargo test --lib -- --ignored plan
    #[test]
    #[ignore]
    fn matches_python_planner_live() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/target/plan_live.json");
        let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).expect("run tools/golden_plan.py --live first")).unwrap();
        check(&cases);
    }
}
