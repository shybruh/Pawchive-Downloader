//! Talking to the archive sites (Pawchive, cum.st): creator names and profiles, post lists, single
//! posts (with Pawchive's temporary links for oversized files), comments, DMs, creator tags,
//! account favorites and the full creator lists the watchlist uses.
//!
//! A port of core/api_client.py. The parsing parts and the live results are checked against the
//! Python client by `tools/parity_api.py` (run with: cargo test --lib -- --ignored sites).

use super::links::{archive_site, ParsedLink};
use regex::Regex;
use reqwest::blocking::{Client, Response};
use scraper::{ElementRef, Html, Selector};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CUMST_SERVICES: &[&str] = &["onlyfans", "fansly", "candfans"];
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const CLIENT_NOTICE: &str = "Pawchive Downloader user here! Love your site. If my client is ever causing server strain, please open an issue on GitHub instead of a hard ban and I'll fix my request pacing immediately.";

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid pattern")
}

macro_rules! cached {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| re($p))
    }};
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn sleep_cancellable(secs: f64, cancel: Option<&AtomicBool>) {
    let mut left = secs;
    while left > 0.0 {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return;
        }
        let step = left.min(0.5);
        std::thread::sleep(Duration::from_secs_f64(step));
        left -= step;
    }
}

pub fn uniform(a: f64, b: f64) -> f64 {
    let mut x = [0u8; 4];
    let _ = getrandom::fill(&mut x);
    a + (b - a) * (u32::from_le_bytes(x) as f64 / u32::MAX as f64)
}

fn is_disabled(host_or_url: &str) -> bool {
    matches!(archive_site(host_or_url), Some("kemono" | "coomer"))
}

/// A bare JWT / session value becomes "session=<value>"
pub fn normalize_cookie(v: &str) -> String {
    let c = v.trim();
    if c.is_empty() {
        String::new()
    } else if (c.starts_with("eyJ") && !c.contains("session=")) || !c.contains('=') {
        format!("session={c}")
    } else {
        c.to_string()
    }
}

// ── Text ────────────────────────────────────────────────────────────────────

const CP1252: [(u8, char); 27] = [
    (0x80, '€'), (0x82, '‚'), (0x83, 'ƒ'), (0x84, '„'), (0x85, '…'), (0x86, '†'), (0x87, '‡'), (0x88, 'ˆ'), (0x89, '‰'),
    (0x8A, 'Š'), (0x8B, '‹'), (0x8C, 'Œ'), (0x8E, 'Ž'), (0x91, '\u{2018}'), (0x92, '\u{2019}'), (0x93, '\u{201C}'),
    (0x94, '\u{201D}'), (0x95, '•'), (0x96, '–'), (0x97, '—'), (0x98, '˜'), (0x99, '™'), (0x9A, 'š'), (0x9B, '›'),
    (0x9C, 'œ'), (0x9E, 'ž'), (0x9F, 'Ÿ'),
];

/// text.encode(enc).decode("utf-8") for latin-1 / cp1252
fn redecode(s: &str, cp1252: bool) -> Option<String> {
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        let n = c as u32;
        let b = if n < 0x80 || (!cp1252 && n <= 0xFF) || (cp1252 && (0xA0..=0xFF).contains(&n)) {
            n as u8
        } else if cp1252 {
            CP1252.iter().find(|(_, ch)| *ch == c).map(|(b, _)| *b)?
        } else {
            return None;
        };
        bytes.push(b);
    }
    String::from_utf8(bytes).ok()
}

/// Display text: entities decoded, mojibake repaired, invisible characters removed, single spaces
pub fn clean_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut s = super::py::html_unescape(text);
    const MARKERS: [&str; 8] = ["ï»¿", "ðŸ", "â€™", "â€œ", "â€”", "Ã©", "Ã", "ð"];
    if MARKERS.iter().any(|m| s.contains(m)) {
        if let Some(fixed) = redecode(&s, false).or_else(|| redecode(&s, true)) {
            s = fixed;
        }
    }
    let s = cached!(r"[\x{feff}\x{200b}-\x{200f}\x{2028}-\x{202f}\x{2060}-\x{206f}\x{00ad}\x{fffd}]").replace_all(&s, "");
    let s = s.replace("ï»¿", "");
    cached!(r"[ \t\r\n]+").replace_all(&s, " ").trim().to_string()
}

// ── HTML helpers (BeautifulSoup equivalents) ───────────────────────────────

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

/// get_text(separator="\n", strip=True)
fn element_text(e: &ElementRef) -> String {
    e.text().map(str::trim).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n")
}

fn has_class_matching(e: &ElementRef, pattern: &Regex) -> bool {
    e.value().classes().any(|c| pattern.is_match(c))
}

// ── The client ──────────────────────────────────────────────────────────────

/// Picks the account cookie saved for a site ("pawchive" / "cumst"), "" when there's none
pub type AccountCookies = Box<dyn Fn(&str) -> String + Send + Sync>;

/// Where the client's log lines go: (level, category, message); level debug / info / warning /
/// error / success, category "api" or "http" (one line per request), as the Python client's
pub type LogSink = Box<dyn Fn(&str, &str, &str) + Send + Sync>;

type Listings = std::sync::Mutex<HashMap<String, (f64, HashMap<(String, String), f64>)>>;

/// The whole-site creator lists, kept a few minutes for every client
fn listing_cache() -> &'static Listings {
    static C: OnceLock<Listings> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn listing_sizes() -> &'static std::sync::Mutex<HashMap<String, usize>> {
    static S: OnceLock<std::sync::Mutex<HashMap<String, usize>>> = OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(HashMap::from([("cum.st".to_string(), 51000)])))
}

/// HTTP clients (with and without the proxy) shared by every SiteClient, so connections are reused
fn shared_clients(proxy: &str) -> (Client, Client) {
    static POOL: OnceLock<std::sync::Mutex<HashMap<String, (Client, Client)>>> = OnceLock::new();
    let mut pool = POOL.get_or_init(Default::default).lock().unwrap();
    pool.entry(proxy.to_string())
        .or_insert_with(|| {
            let build = || {
                let mut b = Client::builder().connect_timeout(Duration::from_secs(20)).pool_max_idle_per_host(4);
                if !proxy.is_empty() {
                    if let Ok(p) = reqwest::Proxy::all(proxy) {
                        b = b.proxy(p);
                    }
                }
                b.build().expect("HTTP client")
            };
            (build(), build())
        })
        .clone()
}

pub struct SiteClient {
    client: Client,
    quick: Client,
    pub general_cookie: String,
    pub user_agent: String,
    account_cookies: AccountCookies,
    /// The last post list stopped early because of an error (not at its end)
    pub last_fetch_failed: std::cell::Cell<bool>,
    log: LogSink,
}

/// Python's HTTP_STATUS_HINTS
fn status_hint(status: u16) -> &'static str {
    match status {
        400 => "Bad Request — malformed URL or invalid query parameters",
        401 => "Unauthorized — session expired or invalid cookie",
        403 => "Forbidden — content locked (Cloudflare block might help to use cookies)",
        404 => "Not Found — creator or post does not exist on this server",
        408 => "Request Timeout — server closed idle connection",
        410 => "Gone — resource was permanently removed",
        416 => "Range Not Satisfiable — byte range exceeds file size (already complete)",
        429 => "Rate Limited — too many requests (429 backoff active)",
        451 => "Unavailable For Legal Reasons — DMCA or regional restriction",
        500 => "Internal Server Error — server-side crash or backend issue",
        502 => "Bad Gateway — upstream server temporarily unreachable",
        503 => "Service Unavailable — server overloaded or undergoing maintenance",
        504 => "Gateway Timeout — upstream proxy timed out",
        520 => "Web Server Returned Unknown Error (Cloudflare 520)",
        _ => "",
    }
}

pub struct Answer {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: String,
}

impl Answer {
    fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }
}

impl SiteClient {
    pub fn new(general_cookie: &str, account_cookies: AccountCookies) -> Self {
        Self::with_proxy(general_cookie, account_cookies, "")
    }

    /// A client whose requests go through `proxy` ("" for none)
    pub fn with_proxy(general_cookie: &str, account_cookies: AccountCookies, proxy: &str) -> Self {
        let (client, quick) = shared_clients(proxy.trim());
        Self {
            client,
            quick,
            general_cookie: general_cookie.trim().to_string(),
            user_agent: DEFAULT_USER_AGENT.into(),
            account_cookies,
            last_fetch_failed: std::cell::Cell::new(false),
            log: Box::new(|_, _, _| {}),
        }
    }

    pub fn set_log(&mut self, log: LogSink) {
        self.log = log;
    }

    fn say(&self, level: &str, message: &str) {
        (self.log)(level, "api", message);
    }

    /// A line about one request
    fn say_http(&self, level: &str, message: &str) {
        (self.log)(level, "http", message);
    }

    /// The login cookie that may go to `url`: only to the archive site it belongs to
    fn cookie_for(&self, url: &str) -> String {
        match archive_site(url) {
            Some(p @ ("pawchive" | "cumst")) => {
                let account = (self.account_cookies)(p);
                normalize_cookie(if account.is_empty() { &self.general_cookie } else { &account })
            }
            _ => String::new(),
        }
    }

    /// GET with the site's headers. Busy (429) and server errors are retried up to 4 times
    /// (waits of 0, 3, 6 and 8 s) unless `quick`. None when there was no answer at all.
    pub fn get(&self, url: &str, timeout: u64, quick: bool) -> Option<Answer> {
        let host = url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("");
        if is_disabled(host) {
            return None;
        }
        let tries = if quick { 1 } else { 5 };
        for attempt in 0..tries {
            if attempt > 0 {
                let wait = if attempt == 1 { 0.0 } else { (1.5 * 2f64.powi(attempt - 1)).min(8.0) };
                std::thread::sleep(Duration::from_secs_f64(wait));
            }
            let mut req = if quick { &self.quick } else { &self.client }
                .get(url)
                .timeout(Duration::from_secs(timeout))
                .header("User-Agent", &self.user_agent)
                .header("Accept", "application/json, text/plain, */*")
                .header("Accept-Language", "en-US,en;q=0.9")
                .header("X-Contact", "https://github.com/shybruh/Pawchive-Downloader")
                .header("X-Client-Notice", CLIENT_NOTICE);
            if archive_site(host).is_some() {
                req = req.header("Referer", format!("https://{host}/"));
            }
            let cookie = self.cookie_for(url);
            if !cookie.is_empty() {
                req = req.header("Cookie", cookie);
            }
            let t0 = std::time::Instant::now();
            match req.send() {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if !quick && attempt + 1 < tries && matches!(status, 429 | 500 | 502 | 503 | 504) {
                        continue;
                    }
                    let a = read(resp);
                    let ms = t0.elapsed().as_secs_f64() * 1000.0;
                    match status {
                        200 | 206 => {
                            // (the size the server announced, as the Python client showed it)
                            let len = a.headers.get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                            let kind: String = a.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("?").chars().take(40).collect();
                            self.say_http("debug", &format!("HTTP {status}  {ms:6.0}ms  {:>5}KB  {kind}  ← {url}", len / 1024))
                        }
                        416 => self.say_http("debug", &format!("HTTP 416 (Already complete)  ← {url}")),
                        403 => self.say_http("warning", &format!("HTTP 403 Forbidden — {}. Add a session cookie in Settings.", status_hint(403))),
                        429 => {
                            let ra = a.headers.get("Retry-After").and_then(|v| v.to_str().ok()).unwrap_or("?").to_string();
                            self.say_http("warning", &format!("HTTP 429 Rate-limited (Retry-After: {ra}s) — backing off…"))
                        }
                        _ => {
                            let hint = status_hint(status);
                            self.say_http("warning", &format!("HTTP {status}  {ms:6.0}ms  {}  ← {url}", if hint.is_empty() { "Unexpected status" } else { hint }))
                        }
                    }
                    return Some(a);
                }
                Err(_) if !quick && attempt + 1 < tries => continue,
                Err(e) => {
                    if e.is_timeout() {
                        self.say_http("error", &format!("Request timed out after {:.0}ms — {url}", t0.elapsed().as_secs_f64() * 1000.0));
                    } else if e.is_connect() {
                        self.say_http("error", &format!("Connection failed (server unreachable?): {e}"));
                    } else {
                        self.say_http("error", &format!("Unexpected HTTP error: {e}"));
                    }
                    return None;
                }
            }
        }
        None
    }

    // ── Creators ─────────────────────────────────────────────────────────────

    pub fn posts_list_url(domain: &str, service: &str, user_id: &str, offset: usize) -> String {
        format!("https://{domain}/api/v1/{service}/user/{user_id}?o={offset}")
    }

    /// The creator's display name from their page's HTML
    pub fn creator_name_from_html(html: &str, user_id: &str) -> Option<String> {
        if html.is_empty() {
            return None;
        }
        let uid = user_id.trim();
        let ok = |n: String| (!n.is_empty() && n != uid).then_some(n);
        let pick = |r: &Regex| r.captures(html).map(|c| clean_text(&c[1]));
        if let Some(n) = pick(cached!(r#"(?i)itemprop=["']name["'][^>]*>(.*?)<"#)).and_then(ok) {
            return Some(n);
        }
        if let Some(n) = pick(cached!(r#"(?i)class=["'][^"']*user-header__name[^"']*["'][^>]*>(.*?)<"#)).and_then(ok) {
            return Some(n);
        }
        if let Some(n) = pick(cached!(r"(?i)<title>\s*Posts of (.*?) from")).and_then(ok) {
            return Some(n);
        }
        if let Some(n) = pick(cached!(r"(?i)<title>\s*(.*?)'s posts")).and_then(ok) {
            return Some(n);
        }
        if let Some(n) = pick(cached!(r#"(?i)<meta\s+property=["']og:title["']\s+content=["'](.*?)["']"#)) {
            if !n.is_empty() && n != uid && !n.to_lowercase().starts_with("posts of") {
                return Some(n);
            }
        }
        None
    }

    /// The creator's profile: the profile API, else their page's HTML, else a post's author name
    pub fn creator_profile(&self, p: &ParsedLink) -> Value {
        self.say("info", &format!("Fetching creator profile: {}/{}", p.service, p.user_id));
        let uid = p.user_id.trim().to_string();
        let family = if CUMST_SERVICES.contains(&p.service.to_lowercase().as_str()) { "cum.st" } else { "pawchive.pw" };
        let mut domains: Vec<String> = Vec::new();
        for d in [p.domain.as_str(), family] {
            if !d.is_empty() && !is_disabled(d) && !domains.iter().any(|x| x == d) {
                domains.push(d.to_string());
            }
        }
        for d in &domains {
            let url = format!("https://{d}/api/v1/{}/user/{}/profile", p.service, p.user_id);
            if let Some(a) = self.get(&url, 10, true).filter(|a| a.status == 200) {
                if let Some(Value::Object(mut data)) = a.json() {
                    let raw = ["displayName", "name", "username"].iter().find_map(|k| data.get(*k).filter(|v| super::filters::truthy(v)).cloned());
                    let name = raw.map(|v| clean_text(v.as_str().unwrap_or(&v.to_string()))).unwrap_or_default();
                    if !name.is_empty() && name != uid {
                        self.say("success", &format!("Creator: {}  service={}  id={}", py_repr(&name), p.service, p.user_id));
                        data.insert("name".into(), json!(name));
                        return Value::Object(data);
                    }
                }
            }
        }
        for d in &domains {
            let url = format!("https://{d}/{}/user/{}", p.service, p.user_id);
            if let Some(a) = self.get(&url, 10, true).filter(|a| a.status == 200) {
                if let Some(name) = Self::creator_name_from_html(&a.body, &uid) {
                    self.say("success", &format!("Creator: {} (from HTML page {d})  service={}  id={}", py_repr(&name), p.service, p.user_id));
                    return json!({"id": p.user_id, "name": name, "service": p.service});
                }
            }
        }
        for d in &domains {
            if let Some(a) = self.get(&Self::posts_list_url(d, &p.service, &p.user_id, 0), 10, true).filter(|a| a.status == 200) {
                let data = a.json().unwrap_or(Value::Null);
                let items = match &data {
                    Value::Object(o) => o.get("posts").and_then(Value::as_array).cloned().unwrap_or_default(),
                    Value::Array(a) => a.clone(),
                    _ => Vec::new(),
                };
                if let Some(first) = items.first().and_then(Value::as_object) {
                    let raw = ["username", "name"].iter().find_map(|k| first.get(*k).filter(|v| super::filters::truthy(v)).cloned());
                    let name = raw.map(|v| clean_text(v.as_str().unwrap_or(&v.to_string()))).unwrap_or_default();
                    if !name.is_empty() && name != uid {
                        self.say("success", &format!("Creator: {} (from post metadata)  service={}", py_repr(&name), p.service));
                        return json!({"id": p.user_id, "name": name, "service": p.service});
                    }
                }
            }
        }
        self.say("warning", &format!("Could not retrieve profile for {}; using ID as name.", p.user_id));
        json!({"id": p.user_id, "name": p.user_id, "service": p.service})
    }

    /// Just the creator's display name (None when only the id is known)
    pub fn creator_name(&self, p: &ParsedLink) -> Option<String> {
        if !p.is_valid {
            return None;
        }
        let profile = self.creator_profile(p);
        let name = ["displayName", "name", "username"].iter().find_map(|k| profile.get(*k).filter(|v| super::filters::truthy(v)))?;
        let name = name.as_str().map(str::to_string).unwrap_or_else(|| name.to_string());
        (name.trim() != p.user_id.trim()).then(|| clean_text(&name))
    }

    // ── Posts ────────────────────────────────────────────────────────────────

    /// {file name: signed temporary link} from a Pawchive post page
    pub fn temporary_links_from_html(html: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for c in cached!(r#"(?:href|src)=["'](https://t1\.pawchive\.pw/f/[^"']+)["']"#).captures_iter(html) {
            let u = c[1].replace("&amp;", "&");
            let last = u.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
            let name = urlencoding::decode(&last).map(|s| s.into_owned()).unwrap_or(last);
            if !name.is_empty() && !out.iter().any(|(n, _)| n == &name) {
                out.push((name, u));
            }
        }
        out
    }

    /// A Pawchive temporary link (or its "temp_expires") has run out
    pub fn temporary_link_expired(f: &Value) -> bool {
        let Some(o) = f.as_object() else { return false };
        let path = o.get("path").and_then(Value::as_str).filter(|p| p.contains("t1.pawchive.pw"));
        let temp = o.get("temp_url").and_then(Value::as_str).filter(|s| !s.is_empty()).or(path).unwrap_or("");
        if temp.contains("?e=") {
            if let Some(c) = cached!(r"[?&]e=(\d+)").captures(temp) {
                if c[1].parse::<f64>().is_ok_and(|e| now() > e) {
                    return true;
                }
            }
        }
        if let Some(exp) = o.get("temp_expires").filter(|v| super::filters::truthy(v)) {
            let s = exp.as_str().map(str::to_string).unwrap_or_else(|| exp.to_string()).replace('Z', "+00:00");
            if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&s) {
                if chrono::Utc::now() > t {
                    return true;
                }
            }
        }
        false
    }

    /// Fills in the temporary links of a Pawchive post's oversized files; returns how many
    pub fn resolve_temporary_links(&self, post: &mut Value, domain: &str) -> usize {
        let Some(obj) = post.as_object_mut() else { return 0 };
        let mut resolved = 0;
        let mark = |t: &mut Map<String, Value>, url: &str| {
            t.insert("path".into(), json!(url));
            t.insert("is_temporary".into(), json!(true));
            t.insert("preview_only".into(), json!(false));
        };
        // 1. temp_url given in the API answer
        let mut fix = |t: &mut Value| {
            let expired = Self::temporary_link_expired(t);
            if let Some(o) = t.as_object_mut() {
                if let Some(u) = o.get("temp_url").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string) {
                    if !expired {
                        mark(o, &u);
                        resolved += 1;
                    }
                }
            }
        };
        if let Some(f) = obj.get_mut("file").filter(|f| f.is_object()) {
            fix(f);
        }
        if let Some(Value::Array(atts)) = obj.get_mut("attachments") {
            for a in atts.iter_mut().filter(|a| a.is_object()) {
                fix(a);
            }
        }
        // 2. Older posts: deferred files whose links are only on the post page
        let is_deferred = |a: &Value| {
            let o = a.as_object();
            o.is_some_and(|o| {
                (super::filters::truthy(o.get("deferred").unwrap_or(&Value::Null)) || super::filters::truthy(o.get("preview_only").unwrap_or(&Value::Null)))
                    && !super::filters::truthy(o.get("is_temporary").unwrap_or(&Value::Null))
                    && !o.get("path").and_then(Value::as_str).is_some_and(|p| p.starts_with("http"))
            })
        };
        let deferred = obj.get("attachments").and_then(Value::as_array).map(|a| a.iter().filter(|x| is_deferred(x)).count()).unwrap_or(0);
        if deferred == 0 {
            return resolved;
        }
        let text = |k: &str| obj.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).filter(|s| !s.is_empty() && s != "null");
        let (Some(pid), Some(svc), Some(user)) = (text("id"), text("service"), text("user")) else { return resolved };
        let d = if domain.contains("pawchive") { domain } else { "pawchive.pw" };
        let page_url = format!("https://{d}/{svc}/user/{user}/post/{pid}");
        self.say("debug", &format!("Fetching post HTML to resolve {deferred} temporary file(s): {page_url}"));
        let Some(page) = self.get(&page_url, 20, false).filter(|a| a.status == 200 && !a.body.is_empty()) else {
            self.say("warning", &format!("Could not fetch HTML for post {pid} to resolve temporary files"));
            return resolved;
        };
        let links = Self::temporary_links_from_html(&page.body);
        if links.is_empty() {
            self.say("info", &format!("Post {pid} has {deferred} deferred attachment(s), but temporary links are expired or unavailable"));
            return resolved;
        }
        let before = resolved;
        let find = |name: &str| {
            links.iter().find(|(n, _)| n == name).or_else(|| links.iter().find(|(n, _)| n.to_lowercase() == name.to_lowercase())).map(|(_, u)| u.clone())
        };
        if let Some(Value::Array(atts)) = obj.get_mut("attachments") {
            for a in atts.iter_mut() {
                if !is_deferred(a) {
                    continue;
                }
                let name = a.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                if let (Some(u), Some(o)) = (find(&name), a.as_object_mut()) {
                    mark(o, &u);
                    resolved += 1;
                }
            }
        }
        if let Some(f) = obj.get_mut("file") {
            let flags = f.as_object().map(|o| {
                (super::filters::truthy(o.get("deferred").unwrap_or(&Value::Null)) || super::filters::truthy(o.get("preview_only").unwrap_or(&Value::Null)))
                    && !super::filters::truthy(o.get("is_temporary").unwrap_or(&Value::Null))
            });
            if flags == Some(true) {
                let name = f.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                if let (Some(u), Some(o)) = (find(&name), f.as_object_mut()) {
                    mark(o, &u);
                    resolved += 1;
                }
            }
        }
        if resolved > before {
            self.say("info", &format!("✨ Resolved {}/{deferred} temporary oversized file(s) from t1.pawchive.pw for post {pid}", resolved - before));
        }
        resolved
    }

    /// One post (or DM) by its link
    pub fn single_post(&self, p: &ParsedLink) -> Option<Value> {
        let post_id = p.post_id.as_deref().filter(|s| !s.is_empty())?;
        let kind = if p.raw_url.to_lowercase().contains("/dm/") { "dm" } else { "post" };
        self.say("info", &format!("Fetching single {kind}: {post_id}"));
        let a = self.get(&format!("https://{}/api/v1/{}/user/{}/{kind}/{post_id}", p.domain, p.service, p.user_id), 15, false)?;
        if a.status != 200 {
            return None;
        }
        let data = a.json()?;
        let mut post = match data {
            Value::Array(items) => items.into_iter().next()?,
            Value::Object(mut o) => match (o.remove("post"), o.remove("dm")) {
                (Some(p @ Value::Object(_)), _) => p,
                (_, Some(d @ Value::Object(_))) => d,
                (post, dm) => {
                    if let Some(v) = post {
                        o.insert("post".into(), v);
                    }
                    if let Some(v) = dm {
                        o.insert("dm".into(), v);
                    }
                    Value::Object(o)
                }
            },
            _ => return None,
        };
        let origin_pawchive = post.get("origin").map(|o| o.to_string().contains("pawchive")).unwrap_or(false);
        if post.is_object() && (p.domain.contains("pawchive") || origin_pawchive) {
            self.resolve_temporary_links(&mut post, &p.domain);
        }
        Some(post)
    }

    pub fn post_comments(&self, domain: &str, service: &str, user_id: &str, post_id: &str) -> Vec<Value> {
        let Some(a) = self.get(&format!("https://{domain}/api/v1/{service}/user/{user_id}/post/{post_id}/comments"), 15, false) else {
            return Vec::new();
        };
        match (a.status, a.json()) {
            (200, Some(Value::Array(c))) => c,
            (200, Some(Value::Object(o))) => o.get("comments").and_then(Value::as_array).cloned().unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// A creator's posts, `page_start`..=`page_end` (pages of the site's size). Stops early at a
    /// smaller page, when the oldest post is before `date_after`, or on cancel.
    /// `progress(page, posts so far, total if known)`.
    #[allow(clippy::too_many_arguments)]
    pub fn user_posts(
        &self, p: &ParsedLink, page_start: usize, page_end: usize, page_size: usize, progress: &dyn Fn(usize, usize, Option<u64>),
        cancel: &AtomicBool, date_after: &str, date_before: &str,
    ) -> Vec<Value> {
        let mut all: Vec<Value> = Vec::new();
        self.last_fetch_failed.set(false);
        let (mut page, mut offset) = (page_start, (page_start.max(1) - 1) * page_size);
        let (mut errors, mut rate_limited, mut largest, mut pages) = (0, 0u32, 0usize, 0usize);
        let mut total: Option<u64> = None;
        let mut cancelled = false;
        self.say("info", &format!("Post enumeration started — {}/{}  pages {page_start}–{page_end}  (page size: {page_size})", p.service, p.user_id));
        while page <= page_end {
            if cancel.load(Ordering::Relaxed) {
                self.say("warning", "Post enumeration cancelled by user.");
                cancelled = true;
                break;
            }
            let url = if p.domain == "cum.st" {
                format!("https://cum.st/api/v1/{}/user/{}/posts?o={offset}&n={page_size}", p.service, p.user_id)
            } else {
                Self::posts_list_url(&p.domain, &p.service, &p.user_id, offset)
            };
            if page == page_start && all.is_empty() {
                progress(page, 0, total);
            }
            let resp = self.get(&url, 25, false);
            if cancel.load(Ordering::Relaxed) {
                self.say("warning", "Post enumeration cancelled by user.");
                cancelled = true;
                break;
            }
            let Some(resp) = resp else {
                errors += 1;
                self.say("warning", &format!("Page {page} request failed ({errors}/3 consecutive errors)"));
                if errors >= 3 {
                    self.say("error", "Stopping enumeration after 3 consecutive failures.");
                    self.last_fetch_failed.set(true);
                    break;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            errors = 0;
            if resp.status == 429 {
                rate_limited += 1;
                if rate_limited > 6 {
                    self.say("error", &format!("Page {page}: still rate limited after 6 waits; stopping here."));
                    self.last_fetch_failed.set(true);
                    break;
                }
                let wait = retry_after(resp.headers.get("Retry-After").and_then(|v| v.to_str().ok()), 5.0 * rate_limited as f64);
                self.say("warning", &format!("Page {page}: Rate limited (429). Pausing {wait:.0}s before retry..."));
                sleep_cancellable(wait, Some(cancel));
                continue;
            }
            rate_limited = 0;
            if resp.status == 403 {
                self.last_fetch_failed.set(true);
                self.say("error", "403 Forbidden — posts are paywalled or cookie is missing/expired. Add your session cookie in Settings → Network.");
                break;
            }
            if resp.status != 200 {
                if matches!(resp.status, 400 | 404) && pages > 0 {
                    self.say("info", &format!("Page {page}: no more posts (HTTP {}).", resp.status));
                } else {
                    self.last_fetch_failed.set(true);
                    self.say("warning", &format!("Page {page}: unexpected HTTP {}, stopping.", resp.status));
                }
                break;
            }
            let data = match serde_json::from_str::<Value>(&resp.body) {
                Ok(d) => d,
                Err(e) => {
                    self.last_fetch_failed.set(true);
                    self.say("error", &format!("Page {page}: JSON parse error — {e}"));
                    break;
                }
            };
            let posts = match &data {
                Value::Object(o) => o
                    .get("posts")
                    .filter(|v| super::filters::truthy(v))
                    .or_else(|| o.get("dms"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                Value::Array(a) => a.clone(),
                _ => Vec::new(),
            };
            if posts.is_empty() {
                self.say("info", &format!("Page {page} returned 0 posts — enumeration complete at offset {offset}."));
                break;
            }
            let batch = posts.len();
            pages += 1;
            self.say("info", &format!("Page {page:3}  offset {offset:5}  +{batch} posts  (running total: {})", all.len() + batch));
            if let Some(Value::Object(o)) = Some(&data) {
                match o.get("total") {
                    Some(Value::Number(n)) if n.as_u64().is_some_and(|t| t > 0) => total = n.as_u64(),
                    Some(Value::String(s)) if !s.trim().is_empty() && s.trim().chars().all(|c| c.is_ascii_digit()) => total = s.trim().parse().ok(),
                    _ => {}
                }
            }
            let oldest = page_oldest_date(&posts);
            all.extend(posts);
            progress(page, all.len(), total);
            largest = largest.max(batch);
            if batch < largest {
                self.say("info", &format!("Partial page ({batch}<{largest}) — reached last page."));
                break;
            }
            if !date_after.is_empty() {
                let (mut a, mut b) = (super::filters::expand_date_start(date_after), if date_before.is_empty() { String::new() } else { super::filters::expand_date_end(date_before) });
                if !a.is_empty() && !b.is_empty() && a > b {
                    std::mem::swap(&mut a, &mut b);
                }
                if !a.is_empty() && !oldest.is_empty() && oldest < a {
                    self.say("info", &format!("Date early-stop: oldest post on page {page} ({oldest}) is before date range start ({a}) — no need to scan further."));
                    break;
                }
            }
            offset += batch;
            page += 1;
            std::thread::sleep(Duration::from_secs_f64(if p.domain.to_lowercase().contains("pawchive") { uniform(0.35, 0.55) } else { 0.15 }));
        }
        let word = if pages == 1 { "page" } else { "pages" };
        if cancelled {
            self.say("info", &format!("Enumeration stopped: {} posts collected from {pages} {word} before cancelling.", all.len()));
        } else {
            self.say("success", &format!("Enumeration done: {} posts collected across {pages} {word}.", all.len()));
        }
        all
    }

    /// A creator's DMs: cum.st's API, or Pawchive's DM pages (HTML)
    #[allow(clippy::too_many_arguments)]
    pub fn user_dms(&self, p: &ParsedLink, page_start: usize, page_end: usize, page_size: usize, progress: &dyn Fn(usize, usize, Option<u64>), cancel: &AtomicBool) -> Vec<Value> {
        let domain = p.domain.to_lowercase();
        let is_cumst = domain.contains("cum.st");
        if !is_cumst && !domain.contains("pawchive") {
            self.say("info", &format!("Direct messages (DMs) are not supported on {}.", p.domain));
            return Vec::new();
        }
        self.say("info", &format!("DM enumeration started — {}/{} on {}", p.service, p.user_id, p.domain));
        let (mut cancelled, mut pages) = (false, 0usize);
        let mut all: Vec<Value> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let (mut page, mut offset) = (page_start, (page_start.max(1) - 1) * page_size);
        let (mut errors, mut rate_limited) = (0, 0u32);
        let mut total: Option<u64> = None;
        while page <= page_end {
            if cancel.load(Ordering::Relaxed) {
                self.say("warning", "DM enumeration cancelled by user.");
                cancelled = true;
                break;
            }
            let url = if is_cumst {
                format!("https://cum.st/api/v1/{}/user/{}/dms?o={offset}&n={page_size}", p.service, p.user_id)
            } else {
                format!("https://pawchive.pw/{}/user/{}/dms?o={offset}", p.service, p.user_id)
            };
            if page == page_start && all.is_empty() {
                progress(page, 0, total);
            }
            let resp = self.get(&url, 25, false);
            if cancel.load(Ordering::Relaxed) {
                self.say("warning", "DM enumeration cancelled by user.");
                cancelled = true;
                break;
            }
            let Some(resp) = resp else {
                errors += 1;
                self.say("warning", &format!("DM Page {page} request failed ({errors}/3 consecutive errors)"));
                if errors >= 3 {
                    self.say("error", "Stopping DM enumeration after 3 consecutive failures.");
                    break;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            errors = 0;
            if resp.status == 429 {
                rate_limited += 1;
                if rate_limited > 6 {
                    self.say("error", &format!("DM Page {page}: rate limited after 6 waits; stopping."));
                    break;
                }
                let wait = retry_after(resp.headers.get("Retry-After").and_then(|v| v.to_str().ok()), 5.0 * rate_limited as f64);
                self.say("warning", &format!("DM Page {page}: Rate limited (429). Pausing {wait:.0}s..."));
                sleep_cancellable(wait, Some(cancel));
                continue;
            }
            rate_limited = 0;
            if matches!(resp.status, 400 | 404) {
                self.say("info", &format!("DM Page {page}: no more DMs (HTTP {}).", resp.status));
                break;
            }
            if resp.status != 200 {
                self.say("warning", &format!("DM Page {page}: unexpected HTTP {}, stopping.", resp.status));
                break;
            }
            let (page_dms, has_next) = if is_cumst {
                let data = match serde_json::from_str::<Value>(&resp.body) {
                    Ok(d) => d,
                    Err(e) => {
                        self.say("error", &format!("DM Page {page}: JSON parse error — {e}"));
                        break;
                    }
                };
                if let Some(t) = data.get("total") {
                    let s = t.as_str().map(str::to_string).unwrap_or_else(|| t.to_string());
                    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
                        total = s.parse().ok();
                    }
                }
                (cumst_dms(&data, p), true)
            } else {
                if total.is_none() {
                    if let Some(c) = cached!(r"DMs\s*\(([0-9,]+)\)").captures(&resp.body) {
                        total = c[1].replace(',', "").parse().ok();
                    }
                }
                pawchive_dms_page(&resp.body, p, offset)
            };
            if page_dms.is_empty() {
                self.say("info", &format!("DM Page {page} returned 0 DMs — enumeration complete at offset {offset}."));
                break;
            }
            let fresh: Vec<Value> = page_dms
                .iter()
                .filter(|d| match d.get("id").and_then(Value::as_str) {
                    Some(id) => seen.insert(id.to_string()),
                    None => true,
                })
                .cloned()
                .collect();
            if fresh.is_empty() {
                self.say("info", &format!("DM Page {page}: no new DMs found (all {} cards were already seen) — pagination complete.", page_dms.len()));
                break;
            }
            let batch = fresh.len();
            all.extend(fresh);
            pages += 1;
            self.say("info", &format!("DM Page {page:3}  offset {offset:5}  +{batch} DMs  (running total: {})", all.len()));
            progress(page, all.len(), total);
            if let Some(t) = total.filter(|t| *t > 0 && all.len() as u64 >= *t) {
                self.say("info", &format!("Reached known total of {t} DMs."));
                break;
            }
            if !is_cumst && !has_next {
                self.say("info", &format!("Last Pawchive DMs page reached ({batch} cards, no subsequent pagination links)."));
                break;
            }
            if !is_cumst && batch < 50 {
                self.say("info", &format!("Last Pawchive DMs page reached ({batch} cards)."));
                break;
            }
            offset += page_dms.len();
            page += 1;
            std::thread::sleep(Duration::from_secs_f64(if domain.contains("pawchive") { uniform(0.35, 0.55) } else { 0.15 }));
        }
        let word = if pages == 1 { "page" } else { "pages" };
        if cancelled {
            self.say("info", &format!("DM enumeration stopped: {} DMs collected from {pages} {word} before cancelling.", all.len()));
        } else {
            self.say("success", &format!("DM enumeration done: {} DMs collected across {pages} {word}.", all.len()));
        }
        all
    }

    /// The creator's tags, most used first
    pub fn creator_tags(&self, p: &ParsedLink) -> Vec<String> {
        let domain = p.domain.to_lowercase();
        if !domain.contains("pawchive") && !domain.contains("cum.st") {
            return Vec::new();
        }
        let url = format!("https://{}/api/v1/{}/user/{}/tags", p.domain, p.service, p.user_id);
        self.say("info", &format!("Fetching creator tags: {url}"));
        let a = self.get(&url, 10, false);
        let got = a.as_ref().map(|a| a.status.to_string()).unwrap_or_else(|| "no response".into());
        let Some(a) = a.filter(|a| a.status == 200) else {
            self.say("warning", &format!("Failed to fetch tags for {} ({}): HTTP {got}", p.user_id, p.service));
            return Vec::new();
        };
        let Some(data) = a.json() else { return Vec::new() };
        let mut tags: Vec<(String, i64)> = Vec::new();
        let count = |v: Option<&Value>| v.and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.trim().parse().ok()))).unwrap_or(0);
        if domain.contains("cum.st") {
            for item in data.get("tags").and_then(Value::as_array).into_iter().flatten() {
                let label = ["label", "slug"].iter().find_map(|k| item.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()));
                if let Some(l) = label {
                    tags.push((l.to_string(), count(item.get("count"))));
                }
            }
        } else {
            for item in data.as_array().into_iter().flatten() {
                if let Some(t) = item.get("tag").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    tags.push((t.to_string(), count(item.get("post_count"))));
                }
            }
        }
        // Python's sort is stable, as is this one
        tags.sort_by(|a, b| b.1.cmp(&a.1));
        self.say("info", &format!("Fetched {} tag(s) for {} [{}]", tags.len(), p.user_id, p.service));
        tags.into_iter().map(|t| t.0).collect()
    }

    /// The signed-in account's favorite posts or creators ("post" / "artist")
    #[allow(clippy::too_many_arguments)]
    pub fn favorites(
        &self, domain: &str, fav_type: &str, page_start: usize, page_end: usize, page_size: usize, progress: &dyn Fn(usize, usize, Option<u64>), cancel: &AtomicBool,
    ) -> Vec<Value> {
        let low = domain.to_lowercase();
        let (is_pawchive, is_cumst) = (low.contains("pawchive"), low.contains("cum.st") || low.contains("cumst"));
        let mut all: Vec<Value> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let (mut page, mut offset, mut errors, mut largest) = (page_start, (page_start.max(1) - 1) * page_size, 0, 0usize);
        self.say("info", &format!("Favorites enumeration started — domain={domain} type={fav_type} pages {page_start}–{page_end}"));
        while page <= page_end {
            if cancel.load(Ordering::Relaxed) {
                self.say("warning", "Favorites enumeration cancelled by user.");
                break;
            }
            let url = if is_cumst {
                format!("https://cum.st/account/bookmarks/{}/__data.json?page={page}", if fav_type == "artist" { "creators" } else { "posts" })
            } else if is_pawchive {
                format!("https://{domain}/favorites?type={fav_type}&o={offset}")
            } else {
                format!("https://{domain}/api/v1/favorites?type={fav_type}&o={offset}")
            };
            progress(page, all.len(), None);
            let resp = self.get(&url, 25, false);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let Some(resp) = resp else {
                errors += 1;
                if errors >= 3 {
                    self.say("error", "Stopping favorites enumeration after 3 consecutive failures.");
                    break;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            errors = 0;
            if matches!(resp.status, 401 | 403) {
                self.say("error", &format!(
                    "HTTP {} Unauthorized when fetching favorites from {domain}. Please log in or add your session cookie in Settings → Accounts & Logins.",
                    resp.status
                ));
                break;
            }
            if resp.status != 200 {
                self.say("warning", &format!("Favorites returned HTTP {}, stopping.", resp.status));
                break;
            }
            let mut cumst_total = None;
            let items: Vec<Value> = if is_cumst {
                let Some(data) = resp.json() else { break };
                let (items, t) = parse_cumst_bookmarks(&data, fav_type);
                cumst_total = t;
                items
            } else if is_pawchive {
                self.pawchive_favorites_page(&resp.body, domain, fav_type)
            } else {
                match resp.json() {
                    Some(Value::Array(a)) => a,
                    Some(Value::Object(o)) => ["posts", "favorites", "artists"]
                        .iter()
                        .find_map(|k| o.get(*k).filter(|v| super::filters::truthy(v)).and_then(Value::as_array).cloned())
                        .unwrap_or_default(),
                    _ => break,
                }
            };
            if items.is_empty() {
                self.say("info", &format!("Page {page}: 0 favorites returned — enumeration complete."));
                break;
            }
            let key = |it: &Value| {
                let f = |k: &str| it.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default();
                if it.is_object() { format!("{}\u{1}{}\u{1}{}", f("service"), f("user"), f("id")) } else { it.to_string() }
            };
            let fresh: Vec<Value> = items.iter().filter(|it| seen.insert(key(it))).cloned().collect();
            if fresh.is_empty() {
                self.say("info", &format!("Page {page}: nothing new — reached end of favorites."));
                break;
            }
            let batch = items.len();
            let new = fresh.len();
            all.extend(fresh);
            self.say("info", &format!("Page {page:3}  offset {offset:5}  +{new} favorite {fav_type}(s) (total: {})", all.len()));
            if let Some(t) = cumst_total.filter(|t| is_cumst && all.len() as i64 >= *t) {
                self.say("info", &format!("Page {page}: reached cum.st total ({}/{t}) — enumeration complete.", all.len()));
                break;
            }
            largest = largest.max(batch);
            if batch < largest {
                self.say("info", &format!("Partial page ({batch}<{largest}) — reached end of favorites."));
                break;
            }
            offset += batch;
            page += 1;
            std::thread::sleep(Duration::from_millis(200));
        }
        self.say("success", &format!("Favorites enumeration done: {} {fav_type}(s) collected.", all.len()));
        all
    }

    fn pawchive_favorites_page(&self, html: &str, domain: &str, fav_type: &str) -> Vec<Value> {
        let doc = Html::parse_document(html);
        let mut items = Vec::new();
        if fav_type == "post" {
            for a in doc.select(&sel("article")).filter(|e| has_class_matching(e, cached!("post-card"))) {
                let attr = |k: &str| a.value().attr(k).map(str::to_string);
                let (Some(pid), Some(svc), Some(usr)) = (attr("data-id"), attr("data-service"), attr("data-user")) else { continue };
                if let Some(Value::Object(full)) = self.get(&format!("https://{domain}/api/v1/{svc}/user/{usr}/post/{pid}"), 15, false).filter(|r| r.status == 200).and_then(|r| r.json()) {
                    items.push(Value::Object(full));
                    continue;
                }
                let title = a.descendants().filter_map(ElementRef::wrap).find(|e| has_class_matching(e, cached!("post-card__title")))
                    .map(|e| e.text().collect::<String>().trim().to_string()).unwrap_or_else(|| format!("Post {pid}"));
                items.push(json!({"id": pid, "user": usr, "service": svc, "title": title}));
            }
        } else {
            for card in doc.select(&sel("a")).filter(|e| has_class_matching(e, cached!("user-card"))) {
                let (Some(cid), Some(csvc)) = (card.value().attr("data-id"), card.value().attr("data-service")) else { continue };
                let name = card.descendants().filter_map(ElementRef::wrap).find(|e| has_class_matching(e, cached!("user-card__name")))
                    .map(|e| e.text().collect::<String>().trim().to_string()).unwrap_or_default();
                let mut avatar = card.select(&sel("img")).next().and_then(|i| i.value().attr("src")).unwrap_or("").to_string();
                if !avatar.is_empty() && !avatar.starts_with("http") {
                    avatar = format!("https://{domain}{avatar}");
                }
                items.push(json!({"id": cid, "service": csvc, "name": name, "avatar": avatar}));
            }
        }
        items
    }

    // ── Whole-site creator lists (the watchlist's "who posted lately") ──────────

    /// Requests needed to read a site's creator list (0: the site has none)
    pub fn creator_listing_cost(&self, domain: &str) -> usize {
        match domain.to_lowercase().as_str() {
            "pawchive.pw" => 1,
            "cum.st" => listing_sizes().lock().unwrap().get("cum.st").copied().unwrap_or(51000) / 45 + 1,
            _ => 0,
        }
    }

    /// {(service, user id): time of the creator's last update} for every creator on the site
    pub fn creator_updates(&self, domain: &str, max_age: f64, cancel: &AtomicBool) -> Option<HashMap<(String, String), f64>> {
        let domain = domain.to_lowercase();
        if !["pawchive.pw", "cum.st"].contains(&domain.as_str()) {
            return None;
        }
        if let Some((t, m)) = listing_cache().lock().unwrap().get(&domain) {
            if now() - t < max_age {
                return Some(m.clone());
            }
        }
        let updates = if domain == "cum.st" { self.cumst_creators(cancel)? } else {
            let a = self.get(&format!("https://{domain}/api/v1/creators"), 60, false).filter(|a| a.status == 200)?;
            let list: Vec<Value> = serde_json::from_str(&a.body).ok()?;
            let mut m = HashMap::new();
            for c in list {
                let Some(id) = c.get("id").filter(|v| !v.is_null()) else { continue };
                let id = id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string());
                let svc = c.get("service").and_then(Value::as_str).unwrap_or("").to_lowercase();
                let upd = c.get("updated").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0.0);
                m.insert((svc, id), upd);
            }
            if m.is_empty() {
                return None;
            }
            m
        };
        listing_cache().lock().unwrap().insert(domain, (now(), updates.clone()));
        Some(updates)
    }

    fn cumst_creators(&self, cancel: &AtomicBool) -> Option<HashMap<(String, String), f64>> {
        let mut updates: HashMap<(String, String), f64> = HashMap::new();
        let (mut offset, mut total, mut failures): (usize, Option<usize>, u32) = (0, None, 0);
        while total.is_none_or(|t| offset < t + 50) {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let Some(a) = self.get(&format!("https://cum.st/api/v1/creators?sort=name&o={offset}"), 30, false).filter(|a| a.status == 200) else {
                failures += 1;
                if failures >= 3 {
                    return None;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            failures = 0;
            let data = a.json()?;
            if let Some(t) = data.get("total").and_then(Value::as_u64) {
                total = Some(t as usize);
                listing_sizes().lock().unwrap().insert("cum.st".into(), t as usize);
            }
            let creators = match &data {
                Value::Object(o) => o.get("creators").and_then(Value::as_array).cloned().unwrap_or_default(),
                Value::Array(a) => a.clone(),
                _ => Vec::new(),
            };
            if creators.is_empty() {
                break;
            }
            for c in &creators {
                let f = |k: &str| c.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0.0);
                let when = f("updated").max(f("contentUpdated"));
                if when <= 0.0 {
                    continue;
                }
                let svc = c.get("service").and_then(Value::as_str).unwrap_or("").to_lowercase();
                for k in ["id", "name", "public_id"] {
                    let Some(v) = c.get(k).filter(|v| super::filters::truthy(v)) else { continue };
                    let ident = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
                    for key in [ident.clone(), ident.to_lowercase()] {
                        let e = updates.entry((svc.clone(), key)).or_insert(0.0);
                        *e = e.max(when);
                    }
                }
            }
            if creators.len() < 50 {
                break;
            }
            offset += 45;
            std::thread::sleep(Duration::from_secs_f64(uniform(0.35, 0.55)));
        }
        (!updates.is_empty()).then_some(updates)
    }
}

/// Python's repr() of a string
pub fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

fn read(resp: Response) -> Answer {
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = resp.text().unwrap_or_default();
    Answer { status, headers, body }
}

/// Retry-After: seconds or an HTTP date, at most two minutes
pub fn retry_after(v: Option<&str>, default: f64) -> f64 {
    let Some(v) = v else { return default };
    if let Ok(s) = v.trim().parse::<f64>() {
        return s.clamp(0.0, 120.0);
    }
    chrono::DateTime::parse_from_rfc2822(v.trim()).map(|t| ((t.timestamp() as f64) - now()).clamp(0.0, 120.0)).unwrap_or(default)
}

/// The oldest "YYYY-MM-DD" among a page's posts ("" when none has a date)
fn page_oldest_date(posts: &[Value]) -> String {
    let mut oldest = String::new();
    for p in posts {
        let pubv = p.get("published").filter(|v| super::filters::truthy(v)).or_else(|| p.get("added")).cloned().unwrap_or(Value::Null);
        let d = match &pubv {
            Value::Number(n) => chrono::DateTime::from_timestamp(n.as_f64().unwrap_or(0.0) as i64, 0)
                .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            Value::Null => String::new(),
            v => {
                let s = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()).trim().to_string();
                if s.contains('T') { s.split('T').next().unwrap_or("").to_string() } else { super::py::take_chars(&s, 10) }
            }
        };
        if !d.is_empty() && (oldest.is_empty() || d < oldest) {
            oldest = d;
        }
    }
    oldest
}

/// cum.st DMs from its API, in the shape of posts
pub fn cumst_dms(data: &Value, p: &ParsedLink) -> Vec<Value> {
    let items = match data {
        Value::Object(o) => o.get("dms").and_then(Value::as_array).cloned().unwrap_or_default(),
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|it| {
            let Value::Object(mut d) = it else { return None };
            let truthy = |d: &Map<String, Value>, k: &str| d.get(k).filter(|v| super::filters::truthy(v)).cloned();
            if truthy(&d, "user").is_none() {
                d.insert("user".into(), json!(p.user_id));
            }
            if truthy(&d, "service").is_none() {
                d.insert("service".into(), json!(p.service));
            }
            d.insert("domain".into(), json!("cum.st"));
            d.insert("is_dm".into(), json!(true));
            let cap = truthy(&d, "captionText").or_else(|| truthy(&d, "contentText")).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default();
            let cap = cap.trim().to_string();
            d.insert("captionText".into(), json!(cap));
            if truthy(&d, "content").is_none() {
                d.insert("content".into(), json!(cap));
            }
            let first: String = cap.split('\n').next().unwrap_or("").chars().take(80).collect::<String>().trim().to_string();
            if truthy(&d, "title").is_none() {
                d.insert("title".into(), json!(if first.is_empty() { "Direct Message".to_string() } else { first }));
            }
            let published = truthy(&d, "published").or_else(|| truthy(&d, "added")).unwrap_or(json!(""));
            d.insert("published".into(), published);
            Some(Value::Object(d))
        })
        .collect()
}

/// One Pawchive DM page (HTML): the DMs, and whether there's a next page
pub fn pawchive_dms_page(html: &str, p: &ParsedLink, offset: usize) -> (Vec<Value>, bool) {
    let doc = Html::parse_document(html);
    let mut has_next = false;
    for a in doc.select(&sel("a[href]")) {
        let href = a.value().attr("href").unwrap_or("");
        if href.contains(&format!("/user/{}/dms", p.user_id)) || href.starts_with("?o=") || href.starts_with("&o=") {
            if let Some(c) = cached!(r"[?&]o=(\d+)").captures(href) {
                if c[1].parse::<usize>().is_ok_and(|o| o > offset) {
                    has_next = true;
                    break;
                }
            }
        }
    }
    let mut out = Vec::new();
    for art in doc.select(&sel("article.dm-card")) {
        let content = art.select(&sel(".dm-card__content")).next();
        let content_html = content.map(|c| c.inner_html().trim().to_string()).unwrap_or_default();
        let content_text = content.map(|c| element_text(&c)).unwrap_or_default();
        let added = art
            .select(&sel(".dm-card__added"))
            .next()
            .and_then(|e| cached!(r"Added:\s*([0-9-]+)").captures(&e.text().collect::<String>()).map(|c| c[1].to_string()))
            .unwrap_or_default();
        let fix = |src: &str| {
            if src.starts_with("http://") || src.starts_with("https://") || src.starts_with('/') && !src.starts_with("//") {
                src.to_string()
            } else if let Some(rest) = src.strip_prefix("//") {
                format!("https://{rest}")
            } else {
                format!("/{src}")
            }
        };
        let file_of = |s: &str, fallback: &str| {
            let n = s.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("").to_string();
            if n.is_empty() { fallback.to_string() } else { n }
        };
        let mut atts = Vec::new();
        for img in art.select(&sel("img")) {
            let Some(src) = img.value().attr("src").filter(|s| !s.is_empty()) else { continue };
            if src.ends_with("logo.png") || src.ends_with("favicon.png") {
                continue;
            }
            let name = file_of(src, "image.jpg");
            atts.push(json!({"name": name, "path": fix(src), "originalFilename": name}));
        }
        for a in art.select(&sel("a")) {
            let Some(href) = a.value().attr("href").filter(|s| !s.is_empty()) else { continue };
            let low = href.to_lowercase();
            if [".jpg", ".png", ".mp4", ".zip", ".rar", "/data/"].iter().any(|e| low.contains(e)) {
                let name = file_of(href, "file.bin");
                atts.push(json!({"name": name, "path": fix(href), "originalFilename": name}));
            }
        }
        let id = art.value().attr("id").filter(|s| !s.is_empty()).or_else(|| art.value().attr("data-id").filter(|s| !s.is_empty())).map(str::to_string).unwrap_or_else(|| {
            let paths = atts.iter().map(|a| a["path"].as_str().unwrap_or("")).collect::<Vec<_>>().join(",");
            let fp = format!("{}:{}:{added}:{content_text}:{paths}", p.service, p.user_id);
            let h: String = Sha256::digest(fp.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
            format!("dm_{}", &h[..16])
        });
        let first: String = content_text.split('\n').next().unwrap_or("").chars().take(80).collect::<String>().trim().to_string();
        out.push(json!({
            "id": id, "user": p.user_id, "service": p.service, "domain": "pawchive.pw", "is_dm": true,
            "title": if first.is_empty() { "Direct Message".to_string() } else { first },
            "captionText": content_text, "content": content_html, "published": added, "attachments": atts,
        }));
    }
    (out, has_next)
}

/// cum.st bookmarks (SvelteKit's "devalue" JSON): (items, total)
pub fn parse_cumst_bookmarks(data: &Value, fav_type: &str) -> (Vec<Value>, Option<i64>) {
    if let Value::Array(a) = data {
        return (a.clone(), None);
    }
    let Some(node) = data.get("nodes").and_then(Value::as_array).filter(|n| n.len() >= 3).and_then(|n| n[2].get("data")).and_then(Value::as_array) else {
        return (Vec::new(), None);
    };
    if node.is_empty() {
        return (Vec::new(), None);
    }
    let resolve = |v: &Value| -> Value {
        match v.as_i64() {
            Some(i) if v.is_i64() && i >= 0 && (i as usize) < node.len() => node[i as usize].clone(),
            _ => v.clone(),
        }
    };
    let Some(root) = node[0].as_object() else { return (Vec::new(), None) };
    let total = root.get("total").map(&resolve).and_then(|t| t.as_i64());
    let mut out = Vec::new();
    let text = |v: Option<&Value>| v.filter(|x| super::filters::truthy(x)).map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).unwrap_or_default();
    if fav_type == "artist" {
        let Some(list) = root.get("creators").map(&resolve).and_then(|l| l.as_array().cloned()) else { return (out, total) };
        for idx in list {
            let Value::Object(c) = resolve(&idx) else { continue };
            let r: Map<String, Value> = c.iter().map(|(k, v)| (k.clone(), resolve(v))).collect();
            let uid = text(r.get("id")).trim().to_string();
            let svc = text(r.get("service")).trim().to_lowercase();
            let name = ["displayName", "name"].iter().find_map(|k| r.get(*k).filter(|v| super::filters::truthy(v)).cloned()).unwrap_or(json!(uid));
            if !uid.is_empty() && !svc.is_empty() {
                out.push(json!({
                    "id": uid, "service": svc, "name": name, "avatar": format!("https://img.cum.st/creator/{svc}/{uid}/avatar.webp"),
                    "domain": "cum.st", "postCount": r.get("postCount").cloned().unwrap_or(json!(0)),
                    "imageCount": r.get("imageCount").cloned().unwrap_or(json!(0)), "videoCount": r.get("videoCount").cloned().unwrap_or(json!(0)),
                }));
            }
        }
    } else {
        let Some(list) = root.get("items").map(&resolve).and_then(|l| l.as_array().cloned()) else { return (out, total) };
        for idx in list {
            let Value::Object(it) = resolve(&idx) else { continue };
            let r: Map<String, Value> = it.iter().map(|(k, v)| (k.clone(), resolve(v))).collect();
            let pid = text(r.get("id")).trim().to_string();
            let user = text(r.get("user").filter(|v| super::filters::truthy(v)).or(r.get("creatorId"))).trim().to_string();
            let svc = text(r.get("service")).trim().to_lowercase();
            let title = ["title", "captionHtml"].iter().find_map(|k| r.get(*k).filter(|v| super::filters::truthy(v)).cloned()).unwrap_or(json!(format!("Post {pid}")));
            if pid.is_empty() {
                continue;
            }
            let mut e = Map::new();
            e.insert("id".into(), json!(pid));
            e.insert("user".into(), json!(user));
            e.insert("service".into(), json!(svc));
            e.insert("title".into(), title);
            e.insert("domain".into(), json!("cum.st"));
            for (k, v) in r {
                e.entry(k).or_insert(v);
            }
            out.push(Value::Object(e));
        }
    }
    (out, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::links::parse;

    trait Lossy { fn from_str_lossy(s: &str) -> Value; }
    impl Lossy for Value { fn from_str_lossy(s: &str) -> Value { serde_json::from_str(s).unwrap() } }

    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
            (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
            (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
            _ => a == b,
        }
    }

    fn run(fun: &str, a: &Value) -> Value {
        let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
        match fun {
            "clean_text" => json!(clean_text(&s("text"))),
            "normalize_cookie" => json!(normalize_cookie(&s("v"))),
            "creator_name_from_html" => json!(SiteClient::creator_name_from_html(&s("html"), &s("uid"))),
            "temporary_links_from_html" => json!(SiteClient::temporary_links_from_html(&s("html"))),
            "temporary_link_expired" => json!(SiteClient::temporary_link_expired(&a["f"])),
            "parse_cumst_bookmarks" => {
                let (items, total) = parse_cumst_bookmarks(&a["data"], &s("kind"));
                json!([items, total])
            }
            "retry_after" => json!(retry_after(a["v"].as_str(), 7.0)),
            "pawchive_dms_page" => {
                let p = parse(&format!("https://pawchive.pw/{}/user/{}", s("service"), s("user")));
                json!(pawchive_dms_page(&s("html"), &p, 0).0)
            }
            other => panic!("no Rust counterpart for {other}"),
        }
    }

    #[test]
    fn matches_python_site_parsing() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/sites.json")).unwrap();
        let mut wrong = Vec::new();
        for c in &cases {
            let fun = c["fn"].as_str().unwrap();
            let got = run(fun, &c["args"]);
            if !same(&got, &c["result"]) {
                wrong.push(format!("{fun} {}\n  python: {}\n  rust:   {}", c["args"], c["result"], got));
            }
        }
        assert!(wrong.is_empty(), "{} of {} cases differ:\n{}", wrong.len(), cases.len(), wrong.join("\n"));
    }

    /// python tools/golden_sites.py --live, then: cargo test --lib -- --ignored sites
    #[test]
    #[ignore]
    fn matches_python_live() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/target/sites_live.json");
        let want: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("run tools/golden_sites.py --live first")).unwrap();
        let c = SiteClient::new("", Box::new(|_| String::new()));
        let p = parse(want["creator"].as_str().unwrap());
        let post = parse(want["post"].as_str().unwrap());
        let cancel = AtomicBool::new(false);
        let mut diffs = Vec::new();
        // Temporary links are signed when asked for: compare them without the signature
        let unsign = |v: &Value| Value::from_str_lossy(cached!(r"\?e=\d+&s=[0-9a-f]+|20\d\d-\d\d-\d\dT[0-9:.]+(\+00:00|Z)?").replace_all(&v.to_string(), "?signed").as_ref());
        let mut check = |what: &str, got: Value, exp: &Value| {
            let (got, exp) = (unsign(&got), unsign(exp));
            if !same(&got, &exp) {
                let (g, e) = (got.to_string(), exp.to_string());
                diffs.push(format!("{what}\n  python: {}\n  rust:   {}", &e[..e.len().min(600)], &g[..g.len().min(600)]));
            }
        };
        check("profile_name", c.creator_profile(&p)["name"].clone(), &want["profile_name"]);
        check("creator_name", json!(c.creator_name(&p)), &want["creator_name"]);
        check("single_post", c.single_post(&post).unwrap_or(json!({})), &want["single_post"]);
        let posts = c.user_posts(&p, 1, 2, 50, &|_, _, _| {}, &cancel, "", "");
        check("posts", json!(posts.iter().map(|x| x["id"].clone()).collect::<Vec<_>>()), &want["posts"]);
        check("posts_failed", json!(c.last_fetch_failed.get()), &want["posts_failed"]);
        check("tags", json!(c.creator_tags(&p)), &want["tags"]);
        check("comments", json!(c.post_comments(&post.domain, &post.service, &post.user_id, post.post_id.as_deref().unwrap())), &want["comments"]);
        check("dms", json!(c.user_dms(&p, 1, 2, 50, &|_, _, _| {}, &cancel)), &want["dms"]);
        check("listing_size", json!(c.creator_updates("pawchive.pw", 600.0, &cancel).map(|m| m.len()).unwrap_or(0)), &want["listing_size"]);
        assert!(diffs.is_empty(), "{}", diffs.join("\n"));
    }
}
