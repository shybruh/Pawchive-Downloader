//! Finding links in posts: cloud-storage links (MEGA, Drive, Dropbox, GoFile…), including ones in
//! clickable words, Markdown, defanged ("hxxps", "[.]") or Base64 form; embedded videos for yt-dlp;
//! and archive passwords written in the post (in English, Japanese, Chinese, Russian, Korean,
//! French or German). A port of services/link_extractor.py.

use super::py::{char_len, html_unescape};
use base64::engine::{general_purpose::GeneralPurposeConfig, DecodePaddingMode, GeneralPurpose};
use base64::Engine;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::OnceLock;

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

const BARE_HOSTS: &str = r"mega\.(?:nz|co\.nz|io)|drive\.google\.com|docs\.google\.com|dropbox\.com|pixeldrain\.com|mediafire\.com|gofile\.io|1fichier\.com|terabox\.com|teraboxapp\.com|1024tera\.com|workupload\.com|qiwi\.gg|(?:bunkr|bunkrr|bunk)\.[a-z0-9]+|balbums\.st|erome\.com";

fn bare_start() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(&format!(r"(?i)^(?:{BARE_HOSTS})/")).unwrap())
}

fn bare_in_text() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(&format!(r#"(?i)(?:^|[\s"'<>(])((?:{BARE_HOSTS})/[^\s"'<>]+)"#)).unwrap())
}

fn ytdlp_patterns() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        [
            r"(?i)https?://(?:www\.)?(?:youtube\.com/(?:watch\?v=|embed/|shorts/|v/)|youtu\.be/)[a-zA-Z0-9_\-]+",
            r#"(?i)https?://(?:www\.)?(?:player\.)?vimeo\.com/(?:video/)?\d+(?:/[0-9a-f]{6,})?(?:\?[^\s"'<>]*)?"#,
            r"(?i)https?://(?:www\.)?picarto\.tv/(?:videopopout|[A-Za-z0-9_]+/videos)/[A-Za-z0-9_\-]+",
            r"(?i)https?://(?:www\.)?streamable\.com/(?:e/)?[a-zA-Z0-9]+",
            r"(?i)https?://(?:www\.)?redgifs\.com/(?:watch/|ifr/)?[a-zA-Z0-9\-]+",
            r"(?i)https?://(?:www\.)?(?:twitter|x)\.com/[a-zA-Z0-9_]+/status/\d+",
            r#"(?i)https?://(?:www\.)?soundcloud\.com/[^\s"'<>]+/[^\s"'<>]+"#,
            r"(?i)https?://(?:www\.)?bilibili\.com/video/[a-zA-Z0-9]+",
            r"(?i)https?://(?:www\.)?dailymotion\.com/video/[a-zA-Z0-9]+",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

/// Hosts that never carry downloadable media (payment, tips, link-in-bio, chat invites)
const NON_MEDIA_EMBED_HOSTS: &[&str] = &[
    "paypal.me", "paypal.com", "ko-fi.com", "buymeacoffee.com", "gumroad.com", "throne.com", "cash.app", "venmo.com",
    "streamlabs.com", "streamelements.com", "linktr.ee", "beacons.ai", "carrd.co", "allmylinks.com", "discord.gg",
    "discord.com", "discordapp.com",
];

const INTERNAL_DOMAINS: &[&str] =
    &["kemono.cr", "kemono.su", "kemono.party", "coomer.st", "coomer.su", "coomer.party", "pawchive.pw", "cum.st", "localhost", "127.0.0.1"];

// ── urllib.parse equivalents ────────────────────────────────────────────────

/// (netloc, path, query) of an absolute URL, like urlsplit. None where urlsplit raises.
fn split_url(u: &str) -> Option<(&str, &str, &str)> {
    let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc = &rest[..end];
    if netloc.contains('[') != netloc.contains(']') {
        return None;
    }
    // A bracketed host must be an IPv6 (or IPvFuture) address
    if let Some((_, b)) = netloc.split_once('[') {
        let inner = b.split(']').next().unwrap_or("");
        if !(inner.starts_with('v') || inner.split('%').next().unwrap_or("").parse::<std::net::Ipv6Addr>().is_ok()) {
            return None;
        }
    }
    let tail = &rest[end..];
    let tail = tail.split('#').next().unwrap_or("");
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    Some((netloc, path, query))
}

/// urlparse(url).hostname
fn hostname(u: &str) -> String {
    let Some((netloc, _, _)) = split_url(u) else { return String::new() };
    let host = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let host = if let Some(inner) = host.strip_prefix('[') { inner.split(']').next().unwrap_or("") } else { host.split(':').next().unwrap_or("") };
    host.to_lowercase()
}

/// urllib.parse.unquote (invalid UTF-8 becomes U+FFFD)
pub fn unquote(s: &str) -> String {
    String::from_utf8_lossy(&urlencoding::decode_binary(s.as_bytes())).into_owned()
}

/// parse_qs(query).get(key) — blank values dropped
fn query_values(query: &str, key: &str) -> Vec<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(_, v)| !v.is_empty())
        .filter(|(k, _)| unquote(&k.replace('+', " ")) == key)
        .map(|(_, v)| unquote(&v.replace('+', " ")))
        .collect()
}

pub fn is_non_media_host(url: &str) -> bool {
    let host = hostname(url);
    NON_MEDIA_EMBED_HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// A candidate link cleaned up (entities, wrapping quotes/brackets, trailing punctuation) and
/// checked; None for non-links, the archive sites' own pages and static assets
pub fn clean_and_normalize_url(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let u = html_unescape(raw.trim());
    let mut u = u.trim_matches(|c| "\"'<>{}[]()".contains(c)).to_string();
    if u.starts_with("//") {
        u = format!("https:{u}");
    } else if bare_start().is_match(&u) {
        u = format!("https://{u}");
    }
    while let Some(last) = u.chars().last() {
        if !".,;:!?)>]}'\"".contains(last) {
            break;
        }
        if last == '!' && (u.contains("mega.nz") || u.contains("mega.io") || u.contains("mega.co.nz")) && u.contains('#') {
            break;
        }
        u.pop();
    }
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return None;
    }
    let (netloc, path, query) = split_url(&u)?;
    let domain = netloc.to_lowercase();
    if domain.is_empty() || !domain.contains('.') {
        return None;
    }
    if INTERNAL_DOMAINS.iter().any(|d| domain == *d || domain.ends_with(&format!(".{d}"))) {
        if path.contains("/external") || query.contains("url=") {
            for k in ["url", "target", "link"] {
                let v = query_values(query, k);
                if let Some(first) = v.first() {
                    return if first.is_empty() { None } else { clean_and_normalize_url(&unquote(first)) };
                }
            }
        }
        return None;
    }
    let low = u.to_lowercase();
    if [".svg", ".ico", ".css", ".js"].iter().any(|e| low.ends_with(e)) {
        return None;
    }
    Some(u)
}

pub fn categorize_url(url: &str) -> &'static str {
    let u = url.to_lowercase();
    let has = |s: &str| u.contains(s);
    if has("mega.nz") || has("mega.co.nz") || has("mega.io") {
        "mega"
    } else if has("drive.google.com") || has("docs.google.com") || has("drive.usercontent.google.com") {
        "gdrive"
    } else if has("dropbox.com") {
        "dropbox"
    } else if has("pixeldrain.com") {
        "pixeldrain"
    } else if has("gofile.io") {
        "gofile"
    } else if has("mediafire.com") {
        "mediafire"
    } else if has("1fichier.com") {
        "1fichier"
    } else if has("terabox.com") || has("teraboxapp.com") || has("1024tera.com") {
        "terabox"
    } else if has("workupload.com") {
        "workupload"
    } else if has("qiwi.gg") {
        "qiwi"
    } else if has("catbox.moe") {
        "catbox"
    } else if has("bunkr") || has("balbums.st") || has("cdn.cr") || has("scdn.st") {
        "bunkr"
    } else if has("erome.com") {
        "erome"
    } else if has("nhentai.net") {
        "nhentai"
    } else if has("saint2.su") {
        "saint2"
    } else if has("simpcity.") {
        "simpcity"
    } else {
        "other"
    }
}

fn str_field<'a>(o: &'a Value, k: &str) -> Option<&'a str> {
    o.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Embedded videos (YouTube, Vimeo, Streamable, RedGifs…) for yt-dlp, sorted
pub fn embed_urls(post: &Value) -> Vec<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    if let Some(e) = post.get("embed").filter(|e| e.is_object()) {
        if let Some(u) = str_field(e, "url").or_else(|| str_field(e, "src")).and_then(clean_and_normalize_url) {
            found.insert(u);
        }
    }
    let text = |k: &str| post.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let html = format!("{}\n{}\n{}", text("content"), text("captionHtml"), text("caption"));
    if !html.trim().is_empty() {
        for c in re!(r#"(?i)<iframe\s+[^<>]*?src=["']([^"']+)["']"#).captures_iter(&html) {
            if let Some(u) = clean_and_normalize_url(&c[1]) {
                found.insert(u);
            }
        }
        for pat in ytdlp_patterns() {
            for m in pat.find_iter(&html) {
                if let Some(u) = clean_and_normalize_url(m.as_str()) {
                    found.insert(u);
                }
            }
        }
    }
    found.into_iter().filter(|u| !is_non_media_host(u)).collect()
}

/// Defanged ("hxxps://", "[.]") and Base64-encoded links
pub fn rescue_obfuscated_and_base64(text: &str) -> Vec<String> {
    let mut rescued = Vec::new();
    if text.is_empty() {
        return rescued;
    }
    let defanged = re!(r"(?i)h[\*_x]{2,4}ps?://").replace_all(text, "https://");
    let defanged = re!(r"(?i)\[\.\]|\(dot\)").replace_all(&defanged, ".").into_owned();
    if defanged != text {
        for pat in [re!(r#"(?i)https?://[^\s"'<>]+"#), re!(r#"(?i)(?:mega\.(?:nz|io)|drive\.google\.com|pixeldrain\.com)/[^\s"'<>]+"#)] {
            rescued.extend(pat.find_iter(&defanged).map(|m| m.as_str().to_string()));
        }
    }
    static B64: OnceLock<GeneralPurpose> = OnceLock::new();
    let engine = B64.get_or_init(|| {
        GeneralPurpose::new(&base64::alphabet::STANDARD, GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true).with_decode_padding_mode(DecodePaddingMode::Indifferent))
    });
    for m in re!(r"(?:[A-Za-z0-9+/]{4}){4,}(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?").find_iter(text) {
        let b = m.as_str();
        if b.len() < 16 || b.len() > 600 {
            continue;
        }
        let Ok(bytes) = engine.decode(b) else { continue };
        let decoded = String::from_utf8_lossy(&bytes).replace('\u{FFFD}', "");
        let decoded = decoded.trim();
        let low = decoded.to_lowercase();
        let hosts = ["http://", "https://", "mega.nz", "mega.io", "drive.google.com", "dropbox.com", "pixeldrain.com", "gofile.io", "mediafire.com", "1fichier.com", "bunkr", "erome.com"];
        if hosts.iter().any(|h| low.contains(h)) {
            rescued.extend(re!(r#"(?i)https?://[^\s"'<>]+"#).find_iter(decoded).map(|m| m.as_str().to_string()));
            if rescued.is_empty() && (decoded.contains("mega.") || decoded.contains("drive.google")) {
                rescued.push(decoded.to_string());
            }
        }
    }
    rescued
}

/// Every external link in text or HTML, by platform ("mega": [...], "other": [...]), each sorted
pub fn links_from_text(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if text.is_empty() {
        return BTreeMap::new();
    }
    let mut cands: Vec<String> = Vec::new();
    let mut group = |r: &Regex| cands.extend(r.captures_iter(text).map(|c| c[1].to_string()));
    group(re!(r#"(?i)<a\s+[^<>]*?href\s*=\s*["']([^"']+)["']"#));
    group(re!(r#"(?i)<a\s+[^<>]*?href\s*=\s*([^\s>"']+)"#));
    group(re!(r#"(?i)<iframe\s+[^<>]*?src\s*=\s*["']([^"']+)["']"#));
    group(re!(r"\[(?:[^\]]*)\]\(([^)\s]+)\)"));
    cands.extend(re!(r#"(?i)https?://[^\s"'<>]+"#).find_iter(text).map(|m| m.as_str().to_string()));
    cands.extend(re!(r#"(?i)(?:^|[\s"'<>(])(//[^\s"'<>]+)"#).captures_iter(text).map(|c| c[1].to_string()));
    cands.extend(bare_in_text().captures_iter(text).map(|c| c[1].to_string()));
    cands.extend(rescue_obfuscated_and_base64(text));
    for raw in cands {
        if let Some(u) = clean_and_normalize_url(&raw) {
            out.entry(categorize_url(&u).to_string()).or_default().insert(u);
        }
    }
    out.into_iter().map(|(k, v)| (k, v.into_iter().collect())).collect()
}

/// All of a post's text that may hold links: title, body, captions, embed, comments, files
pub fn post_link_text(post: &Value, include_main_file: bool) -> String {
    let mut pieces: Vec<String> = Vec::new();
    if let Some(t) = post.get("title").filter(|v| super::filters::truthy(v)) {
        pieces.push(t.as_str().map(str::to_string).unwrap_or_else(|| t.to_string()));
    }
    for k in ["content", "captionHtml", "caption", "description", "body", "text"] {
        if let Some(v) = str_field(post, k) {
            pieces.push(v.to_string());
        }
    }
    match post.get("embed") {
        Some(e @ Value::Object(_)) => {
            for k in ["url", "src", "description", "title"] {
                if let Some(v) = str_field(e, k) {
                    pieces.push(v.to_string());
                }
            }
        }
        Some(Value::String(s)) if !s.is_empty() => pieces.push(s.clone()),
        _ => {}
    }
    if let Some(c) = post.get("comments_text").filter(|v| super::filters::truthy(v)) {
        pieces.push(c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string()));
    }
    if let Some(Value::Array(comments)) = post.get("comments") {
        for c in comments {
            match c {
                Value::Object(_) => {
                    for k in ["content", "message", "text", "comment"] {
                        if let Some(v) = str_field(c, k) {
                            pieces.push(v.to_string());
                        }
                    }
                }
                Value::String(s) => pieces.push(s.clone()),
                _ => {}
            }
        }
    }
    let mut file_links = |f: &Value| {
        for k in ["path", "name"] {
            if let Some(v) = str_field(f, k).filter(|v| v.contains("://") || v.starts_with("//")) {
                pieces.push(v.to_string());
            }
        }
    };
    if let Some(Value::Array(atts)) = post.get("attachments") {
        atts.iter().filter(|a| a.is_object()).for_each(&mut file_links);
    }
    if include_main_file {
        if let Some(f) = post.get("file").filter(|f| f.is_object()) {
            file_links(f);
        }
    }
    pieces.join("\n")
}

pub fn links_from_post(post: &Value) -> BTreeMap<String, Vec<String>> {
    if !post.is_object() {
        return BTreeMap::new();
    }
    links_from_text(&post_link_text(post, true))
}

// ── Passwords ─────────────────────────────────────────────────────────────

fn is_cjk_or_break(c: char) -> bool {
    ('\u{3040}'..='\u{309f}').contains(&c)
        || ('\u{30a0}'..='\u{30ff}').contains(&c)
        || ('\u{4e00}'..='\u{9fff}').contains(&c)
        || ('\u{ac00}'..='\u{d7af}').contains(&c)
        || ('\u{ff00}'..='\u{ffef}').contains(&c)
        || " (['\"-–—:：".contains(c)
}

/// A password as written, cut down to just the password (quotes, CJK sentence endings, polite
/// suffixes…); "" when it doesn't look like one
pub fn clean_password_candidate(raw: &str) -> String {
    let mut val = re!(r"^(?:は|为|是|：|:|:=|–|—|=|\-|\s)+").replace(raw.trim(), "").trim().to_string();
    const PAIRS: [(&str, &str); 12] = [
        ("「", "」"), ("『", "』"), ("【", "】"), ("（", "）"), ("(", ")"), ("[", "]"), ("{", "}"), ("<", ">"),
        ("\u{201c}", "\u{201d}"), ("\u{2018}", "\u{2019}"), ("\"", "\""), ("'", "'"),
    ];
    for (open, close) in PAIRS {
        if val.starts_with(open) {
            if let Some(pos) = val[open.len()..].find(close) {
                val = val[open.len()..open.len() + pos].trim().to_string();
                break;
            }
        }
    }
    if let Some(m) = re!(r"[，、。：；！？※・★☆◆◇▲△▼▽■□●○～~—―\t\r\n]").find(&val) {
        val = val[..m.start()].trim().to_string();
    }
    if let Some(m) = re!(r"^[A-Za-z0-9_!@#$%^&*+=.\-]+").find(&val) {
        let ascii = m.as_str().to_string();
        if val[ascii.len()..].chars().next().is_some_and(is_cjk_or_break) {
            val = ascii;
        }
    }
    for pat in [re!(r"(?:です|になります|となります|をご入力ください|を入力|です。?)$"), re!(r"(?:请勿|祝大家|解压后|提取码|注意).*$")] {
        val = pat.replace_all(&val, "").trim().to_string();
    }
    while let Some(last) = val.chars().last() {
        if !".,;:!?)>]}'\"\u{201c}\u{201d}\u{2018}\u{2019}、。，；：！？※】」』）〕》〉".contains(last) {
            break;
        }
        val.pop();
        val = val.trim().to_string();
    }
    let n = char_len(&val);
    let low = val.to_lowercase();
    if !(2..=64).contains(&n) || low.starts_with("http") || ["null", "none", "password", "undefined", "n/a"].contains(&low.as_str()) {
        return String::new();
    }
    val
}

fn password_patterns() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        [
            r#"(?i)(?:password|passwords|psswd|pswd|passwd|pass|pw|pwd|pd|passcode|zip pass|rar pass|encryption key)\s*[:=\-–—]\s*([^\s<>"\n,;|]{2,64})"#,
            r#"(?i)(?:password|pwd|pw)\s+is\s+([^\s<>"\n,;|]{2,64})"#,
            r"(?i)[\[(](?:password|pass|pw|pwd|pd)\s*[:=]?\s*([^\])\s]+)[\])]",
            r#"(?:パスワード|パス|解凍パス|解凍PW|解凍pass)\s*(?:は|：|:|:=|–|—|=|\-)*\s*([^\s<>"\n,;|]{2,64})"#,
            r"[【「『](?:パスワード|パス|解凍パス|PW|解压密码|密码)\s*[:：=]?\s*([^】」』\s]+)[】」』]",
            r#"(?:解压密码|解壓密碼|解压码|解壓碼|密码|密碼)\s*(?:为|是|：|:|:=|–|—|=|\-)*\s*([^\s<>"\n,;|]{2,64})"#,
            r#"(?i)(?:пароль|пасс)\s*[:=\-–—]?\s*([^\s<>"\n,;|]{2,64})"#,
            r#"(?:비밀번호|비번)\s*[:：=–—]?\s*([^\s<>"\n,;|]{2,64})"#,
            r#"(?i)(?:mot de passe|mdp)\s*[:=\-–—]\s*([^\s<>"\n,;|]{2,64})"#,
            r#"(?i)(?:passwort|kennwort)\s*[:=\-–—]\s*([^\s<>"\n,;|]{2,64})"#,
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

pub struct FoundPassword {
    pub password: String,
    /// Character positions in the (entity-decoded) text
    pub start: usize,
    pub end: usize,
}

pub fn passwords_with_positions(text: &str) -> Vec<FoundPassword> {
    let mut out = Vec::new();
    if text.is_empty() {
        return out;
    }
    let plain = html_unescape(text);
    let mut seen = HashSet::new();
    for pat in password_patterns() {
        for c in pat.captures_iter(&plain) {
            let g = c.get(1).unwrap();
            let val = clean_password_candidate(g.as_str());
            if !val.is_empty() && seen.insert(val.clone()) {
                out.push(FoundPassword { password: val, start: char_len(&plain[..g.start()]), end: char_len(&plain[..g.end()]) });
            }
        }
    }
    out
}

/// The passwords written in a post's text, first-found first
pub fn passwords(text: &str) -> Vec<String> {
    passwords_with_positions(text).into_iter().map(|p| p.password).collect()
}

/// Each link in the text with the password written nearest to it (within 350 characters), else
/// all of the text's passwords
pub fn links_with_details(text: &str) -> Vec<Value> {
    if text.is_empty() {
        return Vec::new();
    }
    let pws = passwords_with_positions(text);
    let all: Vec<String> = pws.iter().map(|p| p.password.clone()).collect();
    let urls: BTreeSet<String> = links_from_text(text).into_values().flatten().collect();
    let char_find = |hay: &str, needle: &str| hay.find(needle).map(|b| char_len(&hay[..b]));
    urls.into_iter()
        .map(|u| {
            let pos = char_find(text, &u).or_else(|| char_find(text, &re!(r"^https?://").replace(&u, "")));
            let mut assigned: Vec<String> = Vec::new();
            let mut source = "post";
            if let (Some(p), false) = (pos, pws.is_empty()) {
                let end = p + char_len(&u);
                let mut best: Option<(usize, &str)> = None;
                for pw in &pws {
                    let d = if pw.start >= end { pw.start - end } else if pw.end <= p { p - pw.end } else { 0 };
                    if d <= 350 && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, &pw.password));
                    }
                }
                if let Some((_, pw)) = best {
                    assigned.push(pw.to_string());
                    source = "proximity";
                }
            }
            if assigned.is_empty() && !all.is_empty() {
                assigned = all.clone();
                source = "post";
            }
            let mut m = Map::new();
            m.insert("url".into(), Value::from(u.clone()));
            m.insert("platform".into(), Value::from(categorize_url(&u)));
            m.insert("passwords".into(), Value::from(assigned));
            m.insert("all_post_passwords".into(), Value::from(all.clone()));
            m.insert("password_source".into(), Value::from(source));
            Value::Object(m)
        })
        .collect()
}
