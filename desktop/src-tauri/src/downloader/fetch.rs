//! Downloading one file of the queue: skip what's already there, find a server that has it
//! (mirrors, a second pass, Pawchive's expiring links, 403/404/429 handling), transfer it into its
//! ".part" file with the chunked transfer engine, check its size and checksum, and give it its
//! real name. A port of FileDownloader._download_single_file and _finalize_part.
//!
//! The answer is (ok, message) with the same messages as the Python downloader, which the queue
//! reads: "429_RATE_LIMIT …", "Disk full …", "Batch paused by user", "Cancelled", "Skipped: …".

use super::filters::{self as f, Options};
use super::links::archive_site;
use super::plan::{is_preview_url, media_candidate_paths, needs_full_size_upgrade, Task};
use super::py::{basename, dirname};
use crate::transfer::download::{self as xfer, Fail, Job, Progress};
use md5::Md5;
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::HeaderMap;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const PART_SUFFIX: &str = ".part";
const BROWSER_UA: &str = super::sites::DEFAULT_USER_AGENT;
const CONTACT: &str = "https://github.com/shybruh/Pawchive-Downloader";
const CLIENT_NOTICE: &str = "Pawchive Downloader user here! Love your site. If my client is ever causing server strain, please open an issue on GitHub instead of a hard ban and I'll fix my request pacing immediately.";
const SERVER_OFF: &str = "This site isn't supported. Pawchive (pawchive.pw) has the same creators and posts — open the same link on pawchive.pw instead.";

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

/// A file's progress in the queue
#[derive(Debug, Clone, Serialize)]
pub struct State {
    /// pending, downloading, completed, skipped, failed, cancelled, paused
    pub status: String,
    pub error_msg: String,
    pub downloaded_bytes: u64,
    pub progress_pct: u32,
    pub speed_bps: u64,
    pub speed_str: String,
    pub eta_str: String,
    pub retry_count: u32,
    pub retry_capped: bool,
    pub http_status: u16,
    pub used_preview: bool,
    /// Servers tried (for the log)
    pub tried_urls: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            status: "pending".into(),
            error_msg: String::new(),
            downloaded_bytes: 0,
            progress_pct: 0,
            speed_bps: 0,
            speed_str: "0 KB/s".into(),
            eta_str: "--".into(),
            retry_count: 0,
            retry_capped: false,
            http_status: 0,
            used_preview: false,
            tried_urls: Vec::new(),
        }
    }
}

impl State {
    pub fn stop_moving(&mut self) {
        self.speed_bps = 0;
        self.speed_str = "0 KB/s".into();
    }
}

pub struct Item {
    pub task: Task,
    pub st: State,
    /// The settings this file was queued with
    pub options: Option<Arc<Options>>,
}

pub type Cell = Arc<Mutex<Item>>;

pub fn cell(task: Task) -> Cell {
    Arc::new(Mutex::new(Item { task, st: State::default(), options: None }))
}

/// What the downloader asks the rest of the app
pub trait Hooks: Send + Sync {
    fn archive_enabled(&self) -> bool {
        false
    }
    fn is_archived(&self, _t: &Task) -> bool {
        false
    }
    /// A file was saved (or found saved): into the download archive
    fn record_file(&self, _t: &Task, _size: u64, _path: &str) {}
    /// A fresh signed link for a Pawchive temporary file whose link ran out
    fn refresh_temporary_link(&self, _t: &Task) -> Option<String> {
        None
    }
    /// The login cookie that may go to `url` (only to the archive site it belongs to)
    fn cookie_for(&self, _url: &str) -> String {
        String::new()
    }
    /// Embedded videos (yt-dlp) and Telegram files: downloaded by the app
    fn special(&self, _c: &Cell, _o: &Options, _ctl: &Ctl) -> (bool, String) {
        (false, "This kind of file can't be downloaded here".into())
    }
    /// After a file is saved: WebP conversion, audio tags, sidecar files
    fn post_process(&self, _t: &Task, _o: &Options) {}
    /// Something about this file changed (status, progress)
    fn changed(&self, _c: &Cell) {}
    /// Download speed limit for all files together, bytes per second (0 = none)
    fn limit_bps(&self) -> f64 {
        0.0
    }
    /// "PawchiveDownloader/<version> (...)": how the app introduces itself to Pawchive
    fn user_agent(&self) -> String {
        format!("PawchiveDownloader/{} ({CONTACT})", env!("CARGO_PKG_VERSION"))
    }
}

/// The queue's switches, shared with the files being downloaded
#[derive(Default)]
pub struct Ctl {
    pub cancel: AtomicBool,
    pub pause: AtomicBool,
    pub paused_batches: Mutex<HashSet<String>>,
    /// Bytes downloaded in this run (all files)
    pub bytes: AtomicU64,
    /// Files found downloaded already, by why ("already on disk": 12)
    pub already_there: Mutex<BTreeMap<String, usize>>,
}

impl Ctl {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn paused(&self) -> bool {
        self.pause.load(Ordering::Relaxed)
    }
    pub fn batch_paused(&self, batch_id: &str) -> bool {
        self.paused_batches.lock().unwrap().contains(batch_id)
    }
    fn count_already(&self, why: &str) {
        *self.already_there.lock().unwrap().entry(why.into()).or_insert(0) += 1;
    }
    /// Sleeps, waking early on cancel; false when cancelled
    fn nap(&self, secs: f64) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(secs.max(0.0));
        while Instant::now() < end {
            if self.cancelled() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50).min(end.saturating_duration_since(Instant::now())));
        }
        !self.cancelled()
    }
    fn sub_bytes(&self, n: u64) {
        let _ = self.bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |b| Some(b.saturating_sub(n)));
    }
}

pub fn format_speed(bps: u64) -> String {
    if bps == 0 {
        "0 KB/s".into()
    } else if bps >= 1024 * 1024 {
        format!("{:.2} MB/s", bps as f64 / (1024.0 * 1024.0))
    } else if bps >= 1024 {
        format!("{:.1} KB/s", bps as f64 / 1024.0)
    } else {
        format!("{bps} B/s")
    }
}

/// Pawchive stores files as /ab/cd/<sha256>.<ext>: the name is the content's SHA-256
pub fn content_hash_from_url(url: &str) -> String {
    if url.is_empty() || is_preview_url(url) || archive_site(url) != Some("pawchive") {
        return String::new();
    }
    let rest = url.split_once("://").map(|x| x.1).unwrap_or(url);
    re!(r"(?i)/[0-9a-f]{2}/[0-9a-f]{2}/([0-9a-f]{64})\.[^/?#]+(?:[?#]|$)").captures(rest).map(|c| c[1].to_lowercase()).unwrap_or_default()
}

/// (start, total) from a Content-Range header; total 0 when unknown
fn content_range(v: &str) -> (Option<u64>, u64) {
    match re!(r"(?i)^\s*bytes\s+(?:(\d+)-\d+|\*)/(\d+|\*)\s*$").captures(v) {
        Some(c) => (c.get(1).and_then(|m| m.as_str().parse().ok()), c[2].parse().unwrap_or(0)),
        None => (None, 0),
    }
}

fn remove_quietly(p: &str) {
    if !p.is_empty() {
        let _ = std::fs::remove_file(p);
    }
}

fn file_size(p: &str) -> Option<u64> {
    std::fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| m.len())
}

fn is_disabled(url: &str) -> bool {
    matches!(archive_site(url), Some("kemono" | "coomer"))
}

/// Free space on the drive holding `dir`
#[cfg(windows)]
fn free_space(dir: &str) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = std::ffi::OsStr::new(dir).encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: a valid NUL-terminated path and out-pointer
    let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
fn free_space(_dir: &str) -> Option<u64> {
    None
}

fn client() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(25))
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(8)
            .build()
            .expect("HTTP client")
    })
}

/// The answer to the first request for a file (its body isn't read: the transfer engine fetches it)
struct Probe {
    status: u16,
    headers: HeaderMap,
    /// Where the request ended up after redirects
    final_url: String,
    /// The headers it was asked with
    sent: Vec<(String, String)>,
}

fn probe(url: &str, headers: &[(String, String)]) -> Option<Probe> {
    let mut req = client().get(url);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.send().ok()?;
    Some(Probe { status: resp.status().as_u16(), headers: resp.headers().clone(), final_url: resp.url().to_string(), sent: headers.to_vec() })
}

fn header<'a>(h: &'a HeaderMap, k: &str) -> &'a str {
    h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// The headers a file server gets: each host its own Referer, Pawchive the app's own name
fn host_headers(url: &str, service: &str, range: Option<&str>, hooks: &dyn Hooks) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = Vec::new();
    if let Some(r) = range {
        h.push(("Range".into(), r.into()));
    }
    let low = url.to_lowercase();
    let mut set = |k: &str, v: &str| h.push((k.into(), v.into()));
    if low.contains("bunkr") || low.contains("cdn.cr") || low.contains("scdn.st") || service == "bunkr" {
        set("User-Agent", BROWSER_UA);
        set("Referer", "https://bunkr.cr/");
        set("Origin", "https://bunkr.cr");
    } else if low.contains("erome") {
        set("User-Agent", BROWSER_UA);
        set("Referer", "https://www.erome.com/");
    } else if low.contains("nhentai") {
        set("User-Agent", BROWSER_UA);
        set("Referer", "https://nhentai.net/");
    } else if low.contains("pawchive") {
        set("User-Agent", &hooks.user_agent());
        set("Referer", "https://pawchive.pw/");
    } else if low.contains("cum.st") {
        set("User-Agent", BROWSER_UA);
        set("Referer", "https://cum.st/");
    } else {
        set("User-Agent", BROWSER_UA);
    }
    set("Accept", "*/*");
    // The login cookie only goes to the archive site it belongs to
    if archive_site(url).is_some() {
        let cookie = hooks.cookie_for(url);
        if cookie.is_empty() {
            set("X-Contact", CONTACT);
            set("X-Client-Notice", CLIENT_NOTICE);
        } else {
            set("Cookie", &cookie);
        }
    }
    h
}

/// Headers without a cookie, for a second try after a 403
fn clean_headers(url: &str, hooks: &dyn Hooks) -> Vec<(String, String)> {
    let p = |k: &str, v: &str| (k.to_string(), v.to_string());
    if url.to_lowercase().contains("pawchive") {
        vec![p("User-Agent", &hooks.user_agent()), p("Referer", "https://pawchive.pw/"), p("Accept", "*/*"), p("X-Contact", CONTACT)]
    } else {
        vec![
            p("User-Agent", BROWSER_UA),
            p("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8"),
            p("Sec-Fetch-Dest", "document"),
            p("Sec-Fetch-Mode", "navigate"),
            p("Sec-Fetch-Site", "none"),
            p("Sec-Fetch-User", "?1"),
            p("Upgrade-Insecure-Requests", "1"),
        ]
    }
}

fn ok_status(s: u16) -> bool {
    matches!(s, 200 | 206)
}

/// Downloads one file of the queue
pub fn download(c: &Cell, o: &Options, hooks: &dyn Hooks, ctl: &Ctl) -> (bool, String) {
    if ctl.cancelled() {
        return (false, "Cancelled".into());
    }
    {
        let mut it = c.lock().unwrap();
        if ctl.batch_paused(&it.task.batch_id) || it.st.status == "paused" {
            it.st.status = "paused".into();
            it.st.stop_moving();
            it.st.eta_str = "Paused".into();
            drop(it);
            hooks.changed(c);
            return (false, "Batch paused by user".into());
        }
    }
    while ctl.paused() {
        std::thread::sleep(Duration::from_millis(300));
        if ctl.cancelled() {
            return (false, "Cancelled".into());
        }
    }

    let task = {
        let mut it = c.lock().unwrap();
        it.task.target_path = super::naming::fit_path_for_windows(&it.task.target_path, super::naming::WINDOWS_MAX_PATH);
        it.st.status = "downloading".into();
        it.task.clone()
    };
    let _ = std::fs::create_dir_all(dirname(&task.target_path));
    hooks.changed(c);
    let filename = basename(&task.target_path).to_string();
    let set = |f: &dyn Fn(&mut Item)| {
        f(&mut c.lock().unwrap());
        hooks.changed(c);
    };

    // Already in the download archive, unless the copy on disk is a small one to replace
    let disk_cands = media_candidate_paths(&task.target_path);
    let on_disk = || disk_cands.iter().find_map(|p| file_size(p).filter(|s| *s > 0).map(|s| (p.clone(), s)));
    let server_size = task.file_size.max(0);
    if hooks.archive_enabled() {
        let upgrade = on_disk().is_some_and(|(p, s)| needs_full_size_upgrade(s as i64, server_size, &task.target_path, o, &p));
        if !upgrade && hooks.is_archived(&task) {
            set(&|it| {
                it.st.status = "completed".into();
                it.st.progress_pct = 100;
                it.st.eta_str = "Done".into();
            });
            ctl.count_already("in the download archive");
            return (true, "Already archived".into());
        }
    }

    // Already on disk (also as .webp/.avif or another video container)
    let found = if o.flag("keep_duplicates") { None } else { on_disk() };
    let mut upgrade_from: Option<String> = None;
    if let Some((path, size)) = found {
        let (keep, why) = f::should_keep_file(&filename, o, Some(size as i64), None);
        if !keep {
            set(&|it| {
                it.st.status = "skipped".into();
                it.st.error_msg = why.clone();
                it.task.file_size = size as i64;
                it.st.downloaded_bytes = size;
                it.st.progress_pct = 100;
                it.st.eta_str = "Skipped".into();
            });
            return (true, format!("Skipped: {why}"));
        }
        if !needs_full_size_upgrade(size as i64, server_size, &task.target_path, o, &path) {
            set(&|it| {
                it.st.status = "completed".into();
                it.st.downloaded_bytes = size;
                it.task.file_size = size as i64;
                it.st.progress_pct = 100;
                it.st.eta_str = "Done".into();
            });
            if hooks.archive_enabled() {
                let mut t = task.clone();
                t.file_size = size as i64;
                hooks.record_file(&t, size, &path);
            }
            ctl.count_already("already on disk");
            return (true, "Already downloaded".into());
        }
        upgrade_from = Some(path);
    }

    // Gentle pacing between Pawchive files
    let is_pawchive = task.url.to_lowercase().contains("pawchive") || task.service.to_lowercase().contains("pawchive");
    if is_pawchive && !ctl.nap(super::sites::uniform(0.25, 0.40)) {
        return (false, "Cancelled".into());
    }

    if task.is_ytdlp || task.is_telegram || task.url.starts_with("tg://") || task.service == "telegram" {
        return hooks.special(c, o, ctl);
    }

    // Continue an interrupted download from its .part file
    let part_path = format!("{}{PART_SUFFIX}", task.target_path);
    let has_journal = Path::new(&format!("{part_path}.chunks")).exists();
    let mut existing = 0u64;
    if !has_journal {
        if let Some(s) = file_size(&part_path) {
            existing = s;
            if server_size > 0 && existing > server_size as u64 {
                remove_quietly(&part_path);
                existing = 0;
            }
        }
    }
    let mut appending = existing > 0;
    let mut resumed = existing > 0 || has_journal;

    // Unsupported sites are skipped, also for files queued before
    let fix = |u: &str| u.replace("e1.cum.st", "cdn.cum.st");
    let all: Vec<String> = std::iter::once(&task.url).chain(&task.fallback_urls).filter(|u| !is_disabled(u)).map(|u| fix(u)).collect();
    {
        let mut it = c.lock().unwrap();
        it.task.url = fix(&it.task.url);
        it.task.fallback_urls = it.task.fallback_urls.iter().map(|u| fix(u)).collect();
    }
    if all.is_empty() {
        return (false, SERVER_OFF.into());
    }
    let allow_fallback = o.flag("fallback_to_thumbnails");
    let thumbs_only = o.flag("download_thumbnails_only");
    let (mut full, mut previews): (Vec<String>, Vec<String>) = if thumbs_only { (all.clone(), Vec::new()) } else { all.iter().cloned().partition(|u| !is_preview_url(u)) };
    if full.is_empty() && !thumbs_only {
        if !allow_fallback {
            return (false, "Preview thumbnail skipped (fallback to thumbnails is disabled)".into());
        }
        full = std::mem::take(&mut previews);
    }

    // Find a server that has the file: two passes over the full-size mirrors, then the preview
    c.lock().unwrap().st.tried_urls.clear();
    let mut passes = vec![full.clone(), full];
    if !previews.is_empty() && allow_fallback {
        passes.push(previews);
    }
    let n_passes = passes.len();
    let mut resp: Option<Probe> = None;
    let mut busy: Option<String> = None; // a server answered 429 (Retry-After)
    let mut url_now = c.lock().unwrap().task.url.clone();
    'passes: for (pass_idx, urls) in passes.iter().enumerate() {
        if pass_idx == 1 && !ctl.nap(1.5) {
            return (false, "Cancelled".into());
        }
        for attempt in urls {
            if ctl.cancelled() {
                return (false, "Cancelled".into());
            }
            {
                let mut it = c.lock().unwrap();
                if !it.st.tried_urls.contains(attempt) {
                    it.st.tried_urls.push(attempt.clone());
                }
            }
            let range = appending.then(|| format!("bytes={existing}-"));
            let headers = host_headers(attempt, &task.service, range.as_deref(), hooks);
            let Some(p) = probe(attempt, &headers) else {
                resp = None;
                continue;
            };
            let s = p.status;
            if matches!(s, 200 | 206 | 416) {
                url_now = attempt.clone();
                resp = Some(p);
                break 'passes;
            }
            if matches!(s, 403 | 404) && attempt.contains("t1.pawchive.pw") {
                // Pawchive's signed links for big files expire: ask the post page for a fresh one
                resp = Some(p);
                if let Some(fresh) = hooks.refresh_temporary_link(&task).filter(|u| u != attempt) {
                    url_now = fresh.clone();
                    c.lock().unwrap().task.url = fresh.clone();
                    if let Some(p2) = probe(&fresh, &headers).filter(|p| matches!(p.status, 200 | 206 | 416)) {
                        resp = Some(p2);
                        break 'passes;
                    }
                }
                continue;
            }
            if s == 404 {
                if pass_idx + 1 < n_passes || urls.len() > 1 {
                    resp = None;
                    continue;
                }
                set(&|it| it.st.http_status = 404);
                return (false, "404 Not Found — file does not exist on server".into());
            }
            if s == 403 {
                // A server may refuse ranges: ask from the start
                if appending {
                    let no_range: Vec<_> = headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("range")).cloned().collect();
                    if let Some(p2) = probe(attempt, &no_range).filter(|p| ok_status(p.status)) {
                        url_now = attempt.clone();
                        appending = false;
                        existing = 0;
                        resp = Some(p2);
                        break 'passes;
                    }
                }
                // Without the cookie, with plain headers
                let clean = clean_headers(attempt, hooks);
                match probe(attempt, &clean) {
                    Some(p2) if ok_status(p2.status) => {
                        url_now = attempt.clone();
                        appending = false;
                        existing = 0;
                        resp = Some(p2);
                        break 'passes;
                    }
                    Some(p2) if p2.status == 403 && urls.len() == 1 => {
                        // One server only: a burst of 403s often passes after a moment
                        for delay in [1.5, 3.0] {
                            if !ctl.nap(delay) {
                                break;
                            }
                            if let Some(p3) = probe(attempt, &clean).filter(|p| ok_status(p.status)) {
                                url_now = attempt.clone();
                                appending = false;
                                existing = 0;
                                resp = Some(p3);
                                break 'passes;
                            }
                        }
                    }
                    _ => {}
                }
                resp = None;
                continue;
            }
            if s == 429 {
                set(&|it| it.st.http_status = 429);
                let ra = header(&p.headers, "Retry-After").trim().to_string();
                let delay = if !ra.is_empty() && ra.chars().all(|c| c.is_ascii_digit()) { ra.parse::<f64>().unwrap_or(2.5).clamp(1.0, 10.0) } else { 2.5 };
                busy = Some(if ra.is_empty() { "30".into() } else { ra });
                resp = None;
                if urls.len() == 1 && !ctl.nap(delay) {
                    return (false, "Cancelled".into());
                }
                continue;
            }
            resp = None;
        }
    }
    let Some(resp) = resp else {
        // Only busy answers: the queue cools down and tries again (the Python downloader called
        // this "Failed to connect", so its 429 cooldown never started)
        if let Some(ra) = busy {
            return (false, format!("429_RATE_LIMIT (Retry-After: {ra}s)"));
        }
        return (false, "Failed to connect to any file server".into());
    };
    {
        let mut it = c.lock().unwrap();
        it.task.url = url_now.clone();
        it.st.used_preview = (is_preview_url(&url_now) || is_preview_url(&resp.final_url)) && !thumbs_only;
    }
    let status = resp.status;

    if status == 416 {
        // Asked to continue past the end: the .part file may already hold the whole file
        let (_, server_total) = content_range(header(&resp.headers, "Content-Range"));
        if existing > 0 && (server_total == existing || (server_total == 0 && server_size as u64 == existing)) {
            set(&|it| {
                it.task.file_size = existing as i64;
                it.st.downloaded_bytes = existing;
            });
            return finalize(c, o, hooks, ctl, &part_path, existing, None, true, upgrade_from.as_deref());
        }
        remove_quietly(&part_path);
        return (false, "The partly downloaded file didn't match the server's copy; it starts over on retry".into());
    }
    match status {
        403 => {
            set(&|it| it.st.http_status = 403);
            return (false, "403 Forbidden — content locked (membership tier access required)".into());
        }
        404 => {
            set(&|it| it.st.http_status = 404);
            return (false, "404 Not Found — file does not exist on any server mirror".into());
        }
        429 => {
            set(&|it| it.st.http_status = 429);
            let ra = header(&resp.headers, "Retry-After");
            return (false, format!("429_RATE_LIMIT (Retry-After: {}s)", if ra.is_empty() { "30" } else { ra }));
        }
        s if !ok_status(s) => {
            set(&|it| it.st.http_status = s);
            return (false, format!("HTTP {s}"));
        }
        _ => {}
    }

    // The file's size, and where a resumed download continues
    let content_length: u64 = header(&resp.headers, "Content-Length").trim().parse().unwrap_or(0);
    let (range_start, range_total) = content_range(header(&resp.headers, "Content-Range"));
    if status == 206 && range_start.is_some_and(|s| s != existing) {
        remove_quietly(&part_path);
        return (false, "The server sent a different part of the file than asked; it starts over on retry".into());
    }
    let total = if status == 206 {
        if range_total > 0 { range_total } else { existing + content_length }
    } else {
        appending = false;
        existing = 0;
        resumed = false;
        if content_length > 0 { content_length } else { server_size as u64 }
    };
    set(&|it| {
        it.task.file_size = total as i64;
        it.st.downloaded_bytes = existing;
    });

    // The size filter, now that the size is known
    if total > 0 {
        let (keep, why) = f::should_keep_file(&filename, o, Some(total as i64), None);
        if !keep {
            remove_quietly(&part_path);
            set(&|it| {
                it.st.status = "skipped".into();
                it.st.error_msg = why.clone();
                it.st.progress_pct = 100;
                it.st.eta_str = "Skipped".into();
            });
            return (true, format!("Skipped: {why}"));
        }
    }

    // Room for the rest of this file plus a margin
    if let Some(free) = free_space(&dirname(&task.target_path)) {
        let needed = (25u64 << 20).max(total.saturating_sub(existing) + (20 << 20));
        if free < needed {
            return (false, format!("Disk full: {} free, this file needs {}", f::format_size_str(free as i64), f::format_size_str(needed as i64)));
        }
    }

    // The transfer: 8 MB chunks recorded as they finish, retries on dropped connections. Pawchive
    // gets one continuous stream per file, other hosts parallel connections for big files.
    let ranges_ok = status == 206 || header(&resp.headers, "Accept-Ranges").to_lowercase().contains("bytes");
    let connections = if is_pawchive || total < 25 << 20 { 1 } else { 4 };
    let send: HashMap<String, String> = resp
        .sent
        .iter()
        .filter(|(k, _)| !["range", "content-length", "connection", "accept-encoding"].contains(&k.to_lowercase().as_str()))
        .cloned()
        .collect();
    let mut restart = !has_journal && !appending;
    let max_size = f::effective_size_limits(o).1.filter(|m| *m > 0).map(|m| m as u64);
    let want_hash = !content_hash_from_url(&url_now).is_empty() || task.expected_sha256.trim().len() == 64;
    let mut aborted: Option<String> = None;
    let mut sha: Option<String> = None;
    loop {
        let job = Job { url: url_now.clone(), part_path: part_path.clone(), headers: send.clone(), total, ranges: ranges_ok, connections, restart, hash: want_hash, limit_bps: hooks.limit_bps() };
        let (result, stop) = run_transfer(c, &job, hooks, ctl, max_size);
        match result {
            Ok(done) => {
                if done.resumed_from > 0 {
                    resumed = true;
                }
                if want_hash && !done.sha256.is_empty() {
                    sha = Some(done.sha256);
                }
                c.lock().unwrap().st.speed_bps = 0;
                break;
            }
            Err(Fail::Cancelled) => match stop.as_deref() {
                Some("Paused") => {
                    // Wait out the pause, then continue from what's on disk
                    while ctl.paused() && !ctl.cancelled() {
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    if ctl.cancelled() {
                        return (false, "Cancelled".into());
                    }
                    restart = false;
                    continue;
                }
                Some("Too big") => {
                    let got = c.lock().unwrap().st.downloaded_bytes;
                    aborted = Some(format!(
                        "File size ({}) exceeds maximum threshold ({})",
                        f::format_size_str(got as i64),
                        f::format_size_str(max_size.unwrap_or(0) as i64)
                    ));
                    break;
                }
                other => return (false, other.unwrap_or("Cancelled").to_string()),
            },
            Err(e) => {
                if e.status() > 0 {
                    set(&|it| it.st.http_status = e.status());
                }
                let mut msg = e.message();
                if msg.is_empty() {
                    msg = "The download didn't finish".into();
                }
                if matches!(e, Fail::Network(_)) {
                    msg += " (it continues from there on retry)";
                }
                return (false, msg);
            }
        }
    }

    let final_size = file_size(&part_path).unwrap_or(0);
    if aborted.is_some() || max_size.is_some_and(|m| final_size > m) {
        remove_quietly(&part_path);
        remove_quietly(&format!("{part_path}.chunks"));
        let got = c.lock().unwrap().st.downloaded_bytes;
        ctl.sub_bytes(got);
        let why = aborted.unwrap_or_else(|| {
            format!("File size ({}) exceeds maximum threshold ({})", f::format_size_str(final_size as i64), f::format_size_str(max_size.unwrap_or(0) as i64))
        });
        set(&|it| {
            it.st.status = "skipped".into();
            it.st.error_msg = why.clone();
            it.st.progress_pct = 100;
            it.st.eta_str = "Skipped".into();
        });
        return (true, format!("Skipped: {why}"));
    }
    if total > 0 && final_size == 0 {
        remove_quietly(&part_path);
        return (false, "Downloaded file is empty (0.00 MB saved)".into());
    }
    if total > 0 && final_size < total && !ctl.cancelled() {
        return (
            false,
            format!("Incomplete download ({} / {}); it continues from there on retry", f::format_size_str(final_size as i64), f::format_size_str(total as i64)),
        );
    }
    // The size the server told us, received exactly: a checksum mismatch then means the site serves
    // another version, not a damaged download
    let size_confirmed = total > 0 && final_size == total && (content_length > 0 || range_total > 0);
    finalize(c, o, hooks, ctl, &part_path, final_size, sha, resumed || !size_confirmed, upgrade_from.as_deref())
}

/// Runs one transfer, reporting progress and stopping it on cancel / pause / size limit.
/// Returns the result and why it was stopped (None when it ended by itself).
fn run_transfer(c: &Cell, job: &Job, hooks: &dyn Hooks, ctl: &Ctl, max_size: Option<u64>) -> (Result<xfer::Done, Fail>, Option<String>) {
    let p = Arc::new(Progress { done: AtomicU64::new(0), total: AtomicU64::new(job.total), cancel: AtomicBool::new(false) });
    let finished = Arc::new(AtomicBool::new(false));
    let stop: Arc<Mutex<Option<String>>> = Arc::default();
    let result = std::thread::scope(|s| {
        let (p2, fin, stop2) = (p.clone(), finished.clone(), stop.clone());
        s.spawn(move || {
            let mut first = true;
            let (mut last_t, mut last_b) = (Instant::now(), 0u64);
            while !fin.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                let done = p2.done.load(Ordering::Relaxed);
                let total = p2.total.load(Ordering::Relaxed);
                {
                    let mut it = c.lock().unwrap();
                    if first {
                        // What was already on disk doesn't count as downloaded in this run
                        first = false;
                        last_b = done;
                    } else {
                        ctl.bytes.fetch_add(done.saturating_sub(it.st.downloaded_bytes), Ordering::Relaxed);
                    }
                    it.st.downloaded_bytes = done;
                    if total > 0 {
                        it.task.file_size = total as i64;
                    }
                    let dt = last_t.elapsed().as_secs_f64();
                    if dt >= 0.5 {
                        it.st.speed_bps = (done.saturating_sub(last_b) as f64 / dt) as u64;
                        it.st.speed_str = format_speed(it.st.speed_bps);
                        if it.task.file_size > 0 {
                            let size = it.task.file_size as u64;
                            it.st.progress_pct = (done as f64 / size as f64 * 100.0) as u32;
                            if it.st.speed_bps > 0 {
                                let sec = size.saturating_sub(done) / it.st.speed_bps;
                                it.st.eta_str = if sec > 60 { format!("{}m {}s", sec / 60, sec % 60) } else { format!("{sec}s") };
                            }
                        }
                        last_t = Instant::now();
                        last_b = done;
                        drop(it);
                        hooks.changed(c);
                    }
                }
                let why = {
                    let it = c.lock().unwrap();
                    // (a file cancelled on its own, e.g. "Cancel remaining" of its post, stops too)
                    if ctl.cancelled() || it.st.status == "cancelled" {
                        Some("Cancelled")
                    } else if ctl.batch_paused(&it.task.batch_id) || it.st.status == "paused" {
                        Some("Batch paused by user")
                    } else if ctl.paused() {
                        Some("Paused")
                    } else if max_size.is_some_and(|m| it.st.downloaded_bytes > m) {
                        Some("Too big")
                    } else {
                        None
                    }
                };
                if let Some(w) = why {
                    *stop2.lock().unwrap() = Some(w.into());
                    p2.cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        let r = xfer::run(job, &p);
        finished.store(true, Ordering::Relaxed);
        r
    });
    let done = p.done.load(Ordering::Relaxed);
    {
        // The last stretch since the reporter's previous look (bytes that were on disk before don't count)
        let mut it = c.lock().unwrap();
        let from = it.st.downloaded_bytes.max(result.as_ref().map(|d| d.resumed_from).unwrap_or(0));
        ctl.bytes.fetch_add(done.saturating_sub(from), Ordering::Relaxed);
        it.st.downloaded_bytes = done;
    }
    let why = stop.lock().unwrap().clone();
    (result, why)
}

/// Checks a finished .part file (size filter, checksum) and gives it its real name
#[allow(clippy::too_many_arguments)]
fn finalize(c: &Cell, o: &Options, hooks: &dyn Hooks, ctl: &Ctl, part_path: &str, final_size: u64, sha: Option<String>, strict: bool, upgrade_from: Option<&str>) -> (bool, String) {
    let task = c.lock().unwrap().task.clone();
    let filename = basename(&task.target_path).to_string();
    let set = |f: &dyn Fn(&mut Item)| {
        f(&mut c.lock().unwrap());
        hooks.changed(c);
    };
    if final_size > 0 {
        let (keep, why) = f::should_keep_file(&filename, o, Some(final_size as i64), None);
        if !keep {
            remove_quietly(part_path);
            ctl.sub_bytes(c.lock().unwrap().st.downloaded_bytes);
            set(&|it| {
                it.st.status = "skipped".into();
                it.st.error_msg = why.clone();
                it.st.progress_pct = 100;
                it.st.eta_str = "Skipped".into();
            });
            return (true, format!("Skipped: {why}"));
        }
    }
    // The checksum, before any post-processing changes the bytes
    let verified = match verify_hash(&task, part_path, sha.as_deref(), strict) {
        Ok(v) => v,
        Err(problem) => {
            remove_quietly(part_path);
        ctl.sub_bytes(c.lock().unwrap().st.downloaded_bytes);
        set(&|it| {
            it.st.downloaded_bytes = 0;
            it.st.progress_pct = 0;
        });
            return (false, problem);
        }
    };
    // One step: an older small copy under the same name stays until the full file replaces it
    if std::fs::rename(part_path, &task.target_path).is_err() {
        let _ = std::fs::remove_file(&task.target_path);
        if let Err(e) = std::fs::rename(part_path, &task.target_path) {
            return (false, format!("Disk/IO error: {e}"));
        }
    }
    set(&|it| {
        it.st.progress_pct = 100;
        it.st.eta_str = "Done".into();
    });
    // An older copy under another name (a .webp preview) is replaced by the full file
    if let Some(old) = upgrade_from.filter(|p| !p.eq_ignore_ascii_case(&task.target_path)) {
        remove_quietly(old);
    }
    hooks.post_process(&task, o);
    let delay = o.to_dict().get("download_delay").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if delay > 0.0 {
        ctl.nap(delay);
    }
    // "Success" — with what checked it, for the log
    (true, verified.map(|a| format!("Success, {a} verified")).unwrap_or_else(|| "Success".into()))
}

/// Err(message) for a damaged file; Ok(Some("SHA-256" / "MD5")) when the checksum matched. Only the content hash in a Pawchive file name counts, and only
/// when `strict` (the size couldn't confirm the file, or pieces of two attempts were joined).
fn verify_hash(t: &Task, path: &str, precomputed: Option<&str>, strict: bool) -> Result<Option<&'static str>, String> {
    let url_hash = content_hash_from_url(&t.url);
    let raw = if url_hash.is_empty() { t.expected_sha256.trim().to_lowercase() } else { url_hash.clone() };
    if raw.is_empty() || !Path::new(path).exists() {
        return Ok(None);
    }
    let hex = raw.chars().all(|c| c.is_ascii_hexdigit());
    let sha = match raw.len() {
        64 if hex => true,
        32 if hex => false,
        _ => return Ok(None),
    };
    let ext = super::py::splitext(&t.target_path.to_lowercase()).1.to_string();
    let is_video = [".mp4", ".mkv", ".webm", ".mov", ".m4v", ".avi"].contains(&ext.as_str());
    if (t.url.contains("cum.st/media/") || t.url.contains("e1.cum.st") || t.url.contains("cdn.cum.st")) && is_video {
        return Ok(None);
    }
    let computed = match precomputed.filter(|_| sha) {
        Some(h) => h.to_lowercase(),
        None => {
            let Ok(mut file) = std::fs::File::open(path) else { return Ok(None) };
            let mut buf = vec![0u8; 1 << 20];
            let mut s256 = Sha256::new();
            let mut m5 = Md5::new();
            loop {
                let Ok(n) = file.read(&mut buf) else { return Ok(None) };
                if n == 0 {
                    break;
                }
                if sha {
                    s256.update(&buf[..n]);
                } else {
                    m5.update(&buf[..n]);
                }
            }
            let bytes: Vec<u8> = if sha { s256.finalize().to_vec() } else { m5.finalize().to_vec() };
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    };
    if computed == raw {
        Ok(Some(if sha { "SHA-256" } else { "MD5" }))
    } else if !url_hash.is_empty() && strict {
        Err("The file arrived damaged (checksum mismatch); it downloads again on retry".into())
    } else {
        Ok(None)
    }
}
