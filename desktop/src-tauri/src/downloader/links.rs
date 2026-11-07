//! Recognising the links people paste: archive-site creators, posts and DMs (Pawchive, cum.st),
//! Telegram, Bunkr, Erome, nHentai, and posts on the original sites (Patreon, Fanbox, …) that
//! the archive mirrors.
//!
//! A port of core/parser.py; `tests/fixtures/links.json` holds the Python parser's answers and the
//! test below checks this one gives the same for every link.

use regex::{Captures, Regex};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParsedLink {
    pub domain: String,
    pub service: String,
    pub user_id: String,
    pub post_id: Option<String>,
    /// "archive", "telegram", "bunkr", "erome", "nhentai" or "native"
    pub provider: String,
    pub is_single_post: bool,
    pub is_dms: bool,
    pub is_valid: bool,
    pub raw_url: String,
    pub error_msg: String,
    pub extra_data: Map<String, Value>,
}

impl ParsedLink {
    fn new(domain: &str, service: &str, user_id: &str, post_id: Option<&str>, raw_url: &str, provider: &str, extra: Map<String, Value>) -> Self {
        let post_id = post_id.map(str::to_string);
        let is_single_post = if provider == "telegram" {
            post_id.as_deref().is_some_and(|p| !p.is_empty())
        } else {
            post_id.as_deref().is_some_and(|p| !p.is_empty()) || matches!(provider, "bunkr" | "erome" | "nhentai")
        };
        let is_dms = extra.get("is_dms").and_then(Value::as_bool).unwrap_or(false);
        Self {
            domain: domain.into(),
            service: service.into(),
            user_id: user_id.into(),
            post_id,
            provider: provider.into(),
            is_single_post,
            is_dms,
            is_valid: true,
            raw_url: raw_url.into(),
            error_msg: String::new(),
            extra_data: extra,
        }
    }

    fn invalid(raw_url: &str, msg: &str) -> Self {
        Self {
            domain: String::new(),
            service: String::new(),
            user_id: String::new(),
            post_id: None,
            provider: "archive".into(),
            is_single_post: false,
            is_dms: false,
            is_valid: false,
            raw_url: raw_url.into(),
            error_msg: msg.into(),
            extra_data: Map::new(),
        }
    }

    /// Not a page on Pawchive / cum.st (Telegram, Bunkr, …)
    #[allow(dead_code)]
    pub fn is_external(&self) -> bool {
        self.provider != "archive"
    }
}

struct Patterns {
    has_scheme: Regex,
    archive: Regex,
    creators: Regex,
    posts: Regex,
    bunkr: Regex,
    balbums: Regex,
    bunkr_cdn: Regex,
    erome: Regex,
    nhentai: Regex,
    patreon: Regex,
    fanbox: Regex,
    fantia: Regex,
    boosty: Regex,
    subscribestar: Regex,
    dlsite: Regex,
    tg_private_post: Regex,
    tg_invite: Regex,
    tg_public_post: Regex,
    tg_channel: Regex,
    dms_tab: Regex,
    dms_path: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| {
        let r = |s: &str| Regex::new(&format!("(?i){s}")).expect("valid pattern");
        Patterns {
            has_scheme: r(r"^https?://"),
            // anchored: Python used re.match for these three
            archive: r(r"^https?://(?:www\.)?([^/]+)/([^/]+)/user/([^/?#]+)(?:/(?:post|posts|dm|dms)(?:/([^/?#]+))?)?"),
            creators: r(r"^https?://(?:www\.)?([^/]+)/(?:creators|artists)/([^/]+)/([^/?#]+)(?:/(?:post|posts|dm|dms)(?:/([^/?#]+))?)?"),
            posts: r(r"^https?://(?:www\.)?([^/]+)/posts/([^/]+)/([^/?#]+)/([^/?#]+)"),
            bunkr: r(r"https?://(?:[a-zA-Z0-9_-]+\.)?(?:bunkr|bunkrr|bunk)\.[a-z0-9]+/(?:a|v|d|f|i|file)/([a-zA-Z0-9_-]+)"),
            balbums: r(r"https?://(?:[a-zA-Z0-9_-]+\.)?balbums\.st(?:/a/([a-zA-Z0-9_-]+))?"),
            bunkr_cdn: r(r"https?://(?:[a-zA-Z0-9_-]+\.)?(?:cdn\.cr|scdn\.st)/storage/media/([a-zA-Z0-9_-]+(?:\.[a-zA-Z0-9]+)?)"),
            erome: r(r"https?://(?:www\.)?erome\.com/a/([a-zA-Z0-9_-]+)"),
            nhentai: r(r"https?://(?:www\.)?nhentai\.net/g/(\d+)"),
            patreon: r(r"https?://(?:www\.)?patreon\.com/posts/(?:[a-zA-Z0-9_-]+-)?(\d+)"),
            fanbox: r(r"https?://(?:(?:www\.)?fanbox\.cc/@([a-zA-Z0-9_-]+)/posts/(\d+)|([a-zA-Z0-9_-]+)\.fanbox\.cc/posts/(\d+))"),
            fantia: r(r"https?://(?:www\.)?fantia\.jp/posts/(\d+)"),
            boosty: r(r"https?://(?:www\.)?boosty\.to/([a-zA-Z0-9_-]+)/posts/([a-zA-Z0-9_-]+)"),
            subscribestar: r(r"https?://(?:www\.)?subscribestar\.(?:adult|com)/posts/(\d+)"),
            dlsite: r(r"https?://(?:www\.)?dlsite\.com/[^/]+/work/=/product_id/([A-Z0-9]+)"),
            tg_private_post: r(r"https?://(?:www\.)?(?:t\.me|telegram\.me)/c/(\d+)/(\d+)"),
            tg_invite: r(r"https?://(?:www\.)?(?:t\.me|telegram\.me)/(?:\+|joinchat/)([a-zA-Z0-9_-]+)"),
            tg_public_post: r(r"https?://(?:www\.)?(?:t\.me|telegram\.me)/(?:s/)?([a-zA-Z0-9_]{4,})/(\d+)"),
            // Python's "$" also matched before one final newline
            tg_channel: r(r"https?://(?:www\.)?(?:t\.me|telegram\.me)/(?:s/)?([a-zA-Z0-9_]{4,})/?\n?$"),
            dms_tab: r(r"[?&]tab=dms?\b"),
            dms_path: r(r"/(?:dms|dm)$"),
        }
    })
}

fn group<'a>(c: &'a Captures, i: usize) -> Option<&'a str> {
    c.get(i).map(|m| m.as_str()).filter(|s| !s.is_empty())
}

/// What urlparse(url).netloc gave: the part between "//" and the next "/", "?" or "#"
fn netloc(url: &str) -> String {
    let rest = url.split_once("//").map(|(_, r)| r).unwrap_or("");
    rest.split(['/', '?', '#']).next().unwrap_or("").to_lowercase()
}

/// Which archive site a host belongs to: "pawchive", "cumst", or one of the unsupported ones
/// (refused after parsing, so they keep their own domain). See core/providers.py.
pub fn archive_site(host: &str) -> Option<&'static str> {
    let mut h = host.trim().to_lowercase();
    if h.contains("://") {
        h = netloc(&h);
    }
    let h = h.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").trim_matches('.').to_string();
    if h.is_empty() {
        return None;
    }
    let labels: Vec<&str> = h.split('.').collect();
    if labels.contains(&"kemono") {
        Some("kemono")
    } else if labels.contains(&"coomer") {
        Some("coomer")
    } else if h == "pawchive.pw" || h.ends_with(".pawchive.pw") {
        Some("pawchive")
    } else if h == "cum.st" || h.ends_with(".cum.st") {
        Some("cumst")
    } else {
        None
    }
}

fn canonical_domain(domain: &str) -> Option<String> {
    match archive_site(domain)? {
        "pawchive" => Some("pawchive.pw".into()),
        "cumst" => Some("cum.st".into()),
        _ => Some(domain.to_lowercase()),
    }
}

pub const NOT_SUPPORTED: &str =
    "Not a supported link (Pawchive, cum.st, Telegram, Bunkr, Erome, nHentai, GoFile, MEGA, Google Drive or Dropbox).";

pub fn parse(input: &str) -> ParsedLink {
    if input.is_empty() {
        return ParsedLink::invalid(input, "URL cannot be empty.");
    }
    let p = patterns();
    let mut url = input.trim().to_string();
    if !p.has_scheme.is_match(&url) {
        url = format!("https://{url}");
    }
    let url = url.as_str();
    let none = Map::new;
    let extra = |pairs: Vec<(&str, Value)>| pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<Map<String, Value>>();

    if let Some(c) = p.tg_private_post.captures(url) {
        let (channel, msg) = (&c[1], &c[2]);
        return ParsedLink::new("t.me", "telegram", channel, Some(msg), url, "telegram", extra(vec![
            ("is_private", json!(true)), ("type", json!("private_post")), ("channel_id", json!(channel)),
            ("message_id", json!(msg.parse::<u64>().unwrap_or(0))),
        ]));
    }
    if let Some(c) = p.tg_invite.captures(url) {
        let hash = &c[1];
        return ParsedLink::new("t.me", "telegram", hash, None, url, "telegram", extra(vec![
            ("is_private", json!(true)), ("type", json!("invite")), ("invite_hash", json!(hash)),
        ]));
    }
    if let Some(c) = p.tg_public_post.captures(url) {
        let (name, msg) = (&c[1], &c[2]);
        return ParsedLink::new("t.me", "telegram", name, Some(msg), url, "telegram", extra(vec![
            ("is_private", json!(false)), ("type", json!("public_post")), ("channel_name", json!(name)),
            ("message_id", json!(msg.parse::<u64>().unwrap_or(0))),
        ]));
    }
    if let Some(c) = p.tg_channel.captures(url) {
        let name = &c[1];
        return ParsedLink::new("t.me", "telegram", name, None, url, "telegram", extra(vec![
            ("is_private", json!(false)), ("type", json!("channel")), ("channel_name", json!(name)),
        ]));
    }
    if let Some(c) = p.nhentai.captures(url) {
        return ParsedLink::new("nhentai.net", "nhentai", &c[1], Some(&c[1]), url, "nhentai", none());
    }
    if let Some(c) = p.bunkr.captures(url) {
        let host = netloc(url);
        let host = if host.is_empty() { "bunkr.cr".to_string() } else { host };
        return ParsedLink::new(&host, "bunkr", "bunkr_user", Some(&c[1]), url, "bunkr", none());
    }
    if let Some(c) = p.balbums.captures(url) {
        return ParsedLink::new("balbums.st", "bunkr", "bunkr_user", Some(group(&c, 1).unwrap_or("album")), url, "bunkr", none());
    }
    if let Some(c) = p.bunkr_cdn.captures(url) {
        let host = netloc(url);
        let host = if host.is_empty() { "cdn.cr".to_string() } else { host };
        return ParsedLink::new(&host, "bunkr", "bunkr_user", Some(&c[1]), url, "bunkr", none());
    }
    if let Some(c) = p.erome.captures(url) {
        return ParsedLink::new("erome.com", "erome", "erome_user", Some(&c[1]), url, "erome", none());
    }

    // Archive-site links (/{service}/user/{id}) — only on the archive sites themselves
    let site_match = p
        .archive
        .captures(url)
        .filter(|c| canonical_domain(&c[1]).is_some())
        .or_else(|| p.creators.captures(url).filter(|c| canonical_domain(&c[1]).is_some()));
    if site_match.is_none() {
        if let Some(c) = p.posts.captures(url) {
            if let Some(domain) = canonical_domain(&c[1]) {
                return ParsedLink::new(&domain, &c[2].to_lowercase(), &c[3], Some(&c[4]), url, "archive", none());
            }
        }
    }
    if let Some(c) = site_match {
        let domain = canonical_domain(&c[1]).unwrap_or_default();
        let post_id = group(&c, 4);
        let clean_path = url.split('?').next().unwrap_or("").trim_end_matches('/');
        let is_dms = post_id.is_none() && (p.dms_tab.is_match(url) || p.dms_path.is_match(clean_path));
        let extra = if is_dms { extra(vec![("is_dms", json!(true))]) } else { none() };
        return ParsedLink::new(&domain, &c[2].to_lowercase(), &c[3], post_id, url, "archive", extra);
    }

    // Posts on the original sites (looked up on Pawchive)
    if let Some(c) = p.patreon.captures(url) {
        return ParsedLink::new("pawchive.pw", "patreon", "", Some(&c[1]), url, "native", none());
    }
    if let Some(c) = p.fanbox.captures(url) {
        let slug = group(&c, 1).or(group(&c, 3)).unwrap_or("");
        let pid = group(&c, 2).or(group(&c, 4)).unwrap_or("");
        return ParsedLink::new("pawchive.pw", "fanbox", slug, Some(pid), url, "native", none());
    }
    if let Some(c) = p.fantia.captures(url) {
        return ParsedLink::new("pawchive.pw", "fantia", "", Some(&c[1]), url, "native", none());
    }
    if let Some(c) = p.boosty.captures(url) {
        return ParsedLink::new("pawchive.pw", "boosty", &c[1], Some(&c[2]), url, "native", none());
    }
    if let Some(c) = p.subscribestar.captures(url) {
        return ParsedLink::new("pawchive.pw", "subscribestar", "", Some(&c[1]), url, "native", none());
    }
    if let Some(c) = p.dlsite.captures(url) {
        return ParsedLink::new("pawchive.pw", "dlsite", "", Some(&c[1]), url, "native", none());
    }
    ParsedLink::invalid(url, NOT_SUPPORTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every link in the reference set gives the same answer as the Python parser
    /// (regenerate with: python tools/golden_links.py).
    #[test]
    fn matches_python_parser() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/links.json")).unwrap();
        assert!(cases.len() >= 100, "the reference set looks truncated");
        let mut wrong = Vec::new();
        for case in &cases {
            let input = case["input"].as_str().unwrap();
            let got = serde_json::to_value(parse(input)).unwrap();
            if got != case["result"] {
                wrong.push(format!("{input:?}\n  python: {}\n  rust:   {}", case["result"], got));
            }
        }
        assert!(wrong.is_empty(), "{} of {} links differ:\n{}", wrong.len(), cases.len(), wrong.join("\n"));
    }
}
