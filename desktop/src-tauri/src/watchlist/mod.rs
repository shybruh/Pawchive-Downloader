//! The watchlist, being ported from core/watchlist_manager.py. First part: the check for a
//! creator's new posts (WatchlistManager.get_posts_since), checked against the Python version by
//! tools/golden_watchlist.py. Second part: the watchlist's database (store.rs). Then the window's
//! list (view.rs) and the creators and their operations (entries.rs).

pub mod check;
pub mod entries;
pub mod store;
pub mod view;

use chrono::TimeZone;
use serde_json::Value;
use std::collections::HashSet;

/// What a creator's entry says about the last download
#[derive(Debug, Default, Clone)]
pub struct Entry {
    pub creator_name: String,
    pub last_post_date: String,
    pub last_post_id: String,
    pub cutoff_day_ids: Vec<String>,
    pub ignored_post_ids: Vec<String>,
}

impl Entry {
    pub fn from_value(v: &Value) -> Self {
        let s = |k: &str| v.get(k).map(text).unwrap_or_default();
        let list = |k: &str| v.get(k).and_then(Value::as_array).map(|a| a.iter().map(text).collect()).unwrap_or_default();
        Self {
            creator_name: s("creator_name"),
            last_post_date: s("last_post_date"),
            last_post_id: s("last_post_id"),
            cutoff_day_ids: list("cutoff_day_ids"),
            ignored_post_ids: list("ignored_post_ids"),
        }
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// int(a) > int(b) for digit strings of any length
fn bigger(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    (a.len(), a) > (b.len(), b)
}

/// Post IDs grow over time; for IDs that aren't plain numbers the order is unknown (False)
pub fn post_id_is_newer(post_id: &str, than: &str) -> bool {
    let (a, b) = (post_id.trim(), than.trim());
    is_digits(a) && is_digits(b) && bigger(a, b)
}

/// The day ("YYYY-MM-DD") of a post's date (a timestamp counts in local time)
pub(crate) fn post_day(p: &Value) -> String {
    let pub_ = p.get("published").filter(|v| truthy(v)).or_else(|| p.get("added").filter(|v| truthy(v)));
    match pub_ {
        Some(Value::Number(n)) => n
            .as_f64()
            .and_then(|t| chrono::Local.timestamp_opt(t.floor() as i64, 0).single())
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        Some(Value::String(s)) => {
            if s.contains('T') {
                s.split('T').next().unwrap_or("").to_string()
            } else {
                s.chars().take(10).collect()
            }
        }
        _ => String::new(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The sort key's first part: published or added or "0" (a timestamp as its local date and time)
fn sort_date(p: &Value) -> String {
    match p.get("published").filter(|v| truthy(v)).or_else(|| p.get("added").filter(|v| truthy(v))) {
        Some(Value::Number(n)) => n
            .as_f64()
            .and_then(|t| chrono::Local.timestamp_opt(t.floor() as i64, 0).single())
            .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_else(|| "0".into()),
        Some(v) => text(v),
        None => "0".into(),
    }
}

/// The check's result: the new posts (oldest first, ignored ones left out), or the page that
/// couldn't be fetched when errors must not end the list early
#[derive(Debug, Default)]
pub struct Since {
    pub posts: Vec<Value>,
    pub failed_page: Option<usize>,
    /// The pages that were asked for
    pub pages: Vec<usize>,
}

/// WatchlistManager.get_posts_since without its name fix-up: reads the creator's post list page
/// by page (`fetch(page) -> (posts, the fetch failed)`) until it reaches the last download.
pub fn posts_since(
    e: &Entry,
    fetch: &mut dyn FnMut(usize) -> (Vec<Value>, bool),
    page_start: usize,
    page_end: usize,
    raise_on_error: bool,
    cancelled: &dyn Fn() -> bool,
    log: &dyn Fn(&str, &str),
) -> Since {
    let mut out = Since::default();
    let cutoff: String = if e.last_post_date.contains('T') {
        e.last_post_date.split('T').next().unwrap_or("").to_string()
    } else {
        e.last_post_date.chars().take(10).collect()
    };
    let cutoff_id = e.last_post_id.clone();
    let mut known: HashSet<String> = e.cutoff_day_ids.iter().cloned().collect();
    if !cutoff_id.is_empty() {
        known.insert(cutoff_id.clone());
    }
    let page_start = page_start.max(1);
    let page_end = page_end.max(page_start);
    let max_pages = page_end.min(100); // (5,000 posts at most)
    let mut new_posts: Vec<Value> = Vec::new();
    let mut largest = 0usize;
    let mut page = page_start;
    while page <= max_pages {
        if cancelled() {
            break;
        }
        out.pages.push(page);
        let (posts, failed) = fetch(page);
        if raise_on_error && failed {
            out.failed_page = Some(page);
            out.posts.clear();
            return out;
        }
        if posts.is_empty() {
            break;
        }
        let mut oldest: Option<String> = None;
        let mut found_cutoff = false;
        let before = new_posts.len();
        for p in &posts {
            let day = post_day(p);
            let id = p.get("id").map(text).unwrap_or_default();
            if !day.is_empty() && oldest.as_ref().is_none_or(|o| day < *o) {
                oldest = Some(day.clone());
            }
            if cutoff.is_empty() {
                new_posts.push(p.clone());
                continue;
            }
            if !day.is_empty() && day > cutoff {
                new_posts.push(p.clone());
            } else if day == cutoff {
                if !id.is_empty() && id == cutoff_id {
                    found_cutoff = true;
                } else if known.contains(&id) {
                    // already downloaded on that day
                } else if !id.is_empty() && !cutoff_id.is_empty() && is_digits(&id) && is_digits(&cutoff_id) {
                    if bigger(&id, &cutoff_id) {
                        new_posts.push(p.clone());
                    }
                } else if !id.is_empty() && !e.cutoff_day_ids.is_empty() {
                    // IDs that aren't numbers: anything not downloaded that day is new
                    new_posts.push(p.clone());
                } else if cutoff_id.is_empty() && known.is_empty() {
                    // the whole day was covered
                } else if cutoff_id.is_empty() || post_id_is_newer(&id, &cutoff_id) {
                    new_posts.push(p.clone());
                }
            }
        }
        // Stop: a smaller page is the last one; the last download was on this page; this page
        // already reaches back past the last download; or nothing on it was new
        largest = largest.max(posts.len());
        if posts.len() < largest || found_cutoff {
            break;
        }
        if let Some(o) = &oldest {
            if !cutoff.is_empty() && (*o < cutoff || (cutoff_id.is_empty() && *o <= cutoff)) {
                break;
            }
        }
        if !cutoff.is_empty() && new_posts.len() == before {
            log("debug", &format!("Watchlist: {} up to date (no posts newer than {cutoff} on page {page}) — stopping check.", crate::downloader::sites::py_repr(&e.creator_name)));
            break;
        }
        page += 1;
    }
    // Oldest first, for downloading in order (a stable sort, like Python's)
    new_posts.sort_by(|a, b| {
        sort_date(a).cmp(&sort_date(b)).then_with(|| text(a.get("id").unwrap_or(&Value::String("0".into()))).cmp(&text(b.get("id").unwrap_or(&Value::String("0".into())))))
    });
    let ignored: HashSet<&String> = e.ignored_post_ids.iter().collect();
    out.posts = new_posts.into_iter().filter(|p| !ignored.contains(&p.get("id").map(text).unwrap_or_default())).collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// python tools/golden_watchlist.py
    #[test]
    fn matches_python_watchlist_check() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/watchlist.json")).unwrap();
        let mut wrong = Vec::new();
        for c in &cases {
            let pages: Vec<Value> = c["pages"].as_array().unwrap().clone();
            let fails: Vec<u64> = c["fail_pages"].as_array().unwrap().iter().map(|p| p.as_u64().unwrap()).collect();
            let mut fetch = |page: usize| -> (Vec<Value>, bool) {
                let posts = pages.get(page - 1).and_then(Value::as_array).cloned().unwrap_or_default();
                if fails.contains(&(page as u64)) { (Vec::new(), true) } else { (posts, false) }
            };
            let r = posts_since(
                &Entry::from_value(&c["entry"]),
                &mut fetch,
                c["page_start"].as_u64().unwrap() as usize,
                c["page_end"].as_u64().unwrap() as usize,
                c["raise_on_error"].as_bool().unwrap(),
                &|| false,
                &|_, _| {},
            );
            let got = match r.failed_page {
                Some(p) => json!({"raised": format!("page {p} of the post list couldn't be fetched"), "pages": r.pages}),
                None => json!({"ids": r.posts.iter().map(|p| p["id"].clone()).collect::<Vec<_>>(), "pages": r.pages}),
            };
            if got != c["want"] {
                wrong.push(format!("{}\n  python: {}\n  rust:   {}", c["name"], c["want"], got));
            }
        }
        assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    #[test]
    fn newer_ids() {
        assert!(post_id_is_newer("100", "99"));
        assert!(!post_id_is_newer("0099", "100"));
        assert!(post_id_is_newer("123456789012345678901234567890", "123456789012345678901234567889"));
        assert!(!post_id_is_newer("abc", "1"));
    }
}
