//! A link's download up to its plan (the bridge's _async_fetch_and_start for the archive sites):
//! the creator's name, the posts (one post, the DMs, or the post pages, with the DMs when they're
//! wanted), only the account's favorite posts in favorite mode, the comments when the character
//! filter reads them, then the download plan. The creator's folder comes from the app once the
//! name is known (it may be the watchlist's). The files go in the queue on the app's side.
//!
//! Call "link_scan": {"link", "site": {"cookie", "user_agent", "proxy"}, "page_start", "page_end",
//! "date_after", "date_before", "dms", "favorites", "comments", "options",
//! "base_dir", "include_service"} →
//! {"status": "cancelled" | "no_posts" | "plan", "creator_name", "posts", "batch_id", "artist_dir", "plan"}

use super::service;
use super::sites::clean_text;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::Ordering;

pub fn call(id: u64, a: &Value) -> Result<Value, String> {
    let c = service::site_client(&a["site"]);
    let cancel = service::cancel_flag(id);
    let cancelled = || cancel.load(Ordering::Relaxed);
    let p = service::parsed_link(&a["link"]);
    let n = |k: &str, d: usize| a[k].as_u64().map(|x| x as usize).unwrap_or(d);
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let flag = |k: &str| a[k].as_bool().unwrap_or(false);
    let none = |status: &str, name: &str| json!({"status": status, "creator_name": name});

    // 1. The creator
    let profile = c.creator_profile(&p);
    let raw = profile.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(&p.user_id).to_string();
    let name = { let t = clean_text(&raw); if t.is_empty() { p.user_id.clone() } else { t } };
    if cancelled() {
        return Ok(none("cancelled", &name));
    }

    // 2. The posts
    let (page_start, page_end) = (n("page_start", 1), n("page_end", 999_999));
    let quiet = |_: usize, _: usize, _: Option<u64>| {};
    let mut posts: Vec<Value> = if p.is_single_post {
        c.single_post(&p).into_iter().collect()
    } else if p.is_dms {
        c.user_dms(&p, page_start, page_end, 50, &quiet, &cancel)
    } else {
        let mut v = c.user_posts(&p, page_start, page_end, 50, &quiet, &cancel, &s("date_after"), &s("date_before"));
        if flag("dms") {
            service::log_app("info", &format!("Direct messages enabled: scanning DMs for {name}..."), "downloader", "");
            v.extend(c.user_dms(&p, page_start, page_end, 50, &quiet, &cancel));
        }
        if flag("favorites") && !v.is_empty() {
            service::log_app("info", &format!("⭐ Favorite Mode active: Filtering {} post(s) against account favorites...", v.len()), "downloader", "");
            let favs: HashSet<String> = c.favorites(&p.domain, "post", 1, 999_999, 50, &quiet, &cancel).iter()
                .filter_map(|f| f.get("id").filter(|i| !i.is_null()).map(id_text)).filter(|i| !i.is_empty()).collect();
            let before = v.len();
            v.retain(|post| favs.contains(&id_text(post.get("id").unwrap_or(&Value::Null))));
            service::log_app("info", &format!("⭐ Favorite Mode filter: {} of {before} post(s) matched user favorites.", v.len()), "downloader", "");
        }
        v
    };
    if cancelled() {
        return Ok(none("cancelled", &name));
    }
    if posts.is_empty() {
        service::log_app("warning", &format!("No posts found for {name} ({}).", p.service), "api", "");
        return Ok(none("no_posts", &name));
    }
    if flag("comments") {
        for post in posts.iter_mut() {
            if cancelled() {
                break;
            }
            let pid = id_text(post.get("id").unwrap_or(&Value::Null));
            if pid.is_empty() {
                continue;
            }
            let text: Vec<String> = c.post_comments(&p.domain, &p.service, &p.user_id, &pid).iter()
                .filter_map(|m| m.as_object().map(|o| o.get("content").and_then(Value::as_str).unwrap_or("").to_string()))
                .collect();
            if let Some(o) = post.as_object_mut() {
                o.insert("comments_text".into(), json!(text.join("\n")));
            }
        }
    }
    if cancelled() {
        return Ok(none("cancelled", &name));
    }

    // 3. The plan: a post's own batch, else the creator's (in the watchlist's folder when it has one)
    let (batch, artist) = if p.is_single_post && p.post_id.as_deref().is_some_and(|i| !i.is_empty()) {
        (format!("post_{}_{}_{}", p.service, p.user_id, p.post_id.as_deref().unwrap_or("")), String::new())
    } else {
        let dir = service::ask_app("link_artist_dir", json!([p.service, p.user_id, name]))?;
        (format!("artist_{}_{}", p.service, p.user_id), dir.as_str().unwrap_or("").to_string())
    };
    service::log_app("info", &format!("Structuring download plan for {} posts...", posts.len()), "downloader", "");
    let args = json!({
        "options": a["options"], "cookie": a["site"]["cookie"],
        "request": {"creator_name": name, "service": p.service, "domain": p.domain, "base_dir": a["base_dir"],
                    "batch_id": batch, "artist_dir": artist, "user_id": p.user_id,
                    "include_service": a["include_service"].as_bool().unwrap_or(true)},
    });
    let plan = service::plan_build(id, &args, &posts, Some(cancel.clone()));
    if plan.cancelled {
        return Ok(none("cancelled", &name));
    }
    Ok(json!({
        "status": "plan",
        "creator_name": name,
        "posts": posts,
        "batch_id": batch,
        "artist_dir": artist,
        "plan": plan_value(&plan),
    }))
}

/// A plan as the app takes it (RustDownloader.tasks_from_plan)
pub fn plan_value(plan: &super::plan::Plan) -> Value {
    json!({
        "tasks": plan.tasks,
        "pending_post_infos": plan.pending_post_infos,
        "write_now": plan.write_now,
        "harvested_links": plan.harvested_links,
        "harvested_records": plan.harvested_records,
        "not_queued": plan.not_queued,
        "renamed": plan.renamed,
        "cancelled": plan.cancelled,
    })
}

/// A post or favorite id as text (they come as numbers or strings)
fn id_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ── After a link's files are planned ──────────────────────────────────────────────────────────

/// The folder a link's creator is added to the Watchlist with: the creator folder above the first
/// file (under either name, or "<name> [<any site>]"), else theirs in the download folder (the
/// bridge's extract_artist_folder_from_path and its caller)
pub fn watchlist_folder(name: &str, service: &str, artist_dir: &str, first_target: &str, download_dir: &str, include_service: bool) -> String {
    use super::naming::{clean_filesystem_text, is_creator_folder, pick_creator_folder};
    use super::py::{abspath, basename, dirname};
    if !artist_dir.is_empty() {
        return artist_dir.to_string();
    }
    let clean = clean_filesystem_text(name, 80, "creator");
    let low = clean.to_lowercase();
    let pick = |parent: &str| pick_creator_folder(std::path::Path::new(parent), &clean, service, include_service).to_string_lossy().into_owned();
    if first_target.is_empty() {
        return pick(download_dir);
    }
    let mut cur = dirname(&abspath(first_target));
    while !cur.is_empty() && cur != dirname(&cur) {
        let base = basename(&cur);
        if is_creator_folder(base, &clean, service, include_service) || (!service.is_empty() && base.to_lowercase().starts_with(&format!("{low} ["))) {
            return cur;
        }
        cur = dirname(&cur);
    }
    if !download_dir.is_empty() { pick(download_dir) } else { dirname(first_target) }
}

/// Call "link_queued": a link's files are going in the queue. The History tab's session is
/// recorded, and (a creator's link) the creator added to the Watchlist with their folder.
/// {"creator_name", "user_id", "url", "watchlist_url", "service", "domain", "files", "track", "artist_dir",
/// "first_target", "download_dir", "include_service", "options"} → {"history", "watchlist"}
pub fn queued(a: &Value) -> Result<Value, String> {
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let name = { let n = s("creator_name"); if n.is_empty() { s("user_id") } else { n } };
    let recorded = crate::history::record_session(&s("creator_name"), &s("url"), &s("service"), a["files"].as_u64().unwrap_or(0) as usize);
    let mut added = Value::Null;
    if a["track"].as_bool().unwrap_or(false) && !s("user_id").is_empty() {
        let inc = a["include_service"].as_bool().unwrap_or(true);
        let folder = watchlist_folder(&name, &s("service"), &s("artist_dir"), &s("first_target"), &s("download_dir"), inc);
        let opts = if a["options"].is_object() { a["options"].clone() } else { json!({}) };
        added = crate::watchlist::entries::change_app_list("add_entry", |w| {
            w.add_entry(&{ let u = s("watchlist_url"); if u.is_empty() { s("url") } else { u } }, &name, &s("user_id"), &s("service"), &s("domain"), "", "", true, &folder, &opts, false, inc)
        }).map_or(Value::Null, |b| json!(b));
    }
    Ok(json!({"history": recorded, "watchlist": added}))
}

#[cfg(test)]
mod queued_tests {
    use super::watchlist_folder;
    use serde_json::Value;

    #[test]
    fn folders_match_python() {
        let fx: Value = serde_json::from_str(include_str!("../../tests/fixtures/history.json")).unwrap();
        let d = std::env::temp_dir().join(format!("pawchive_qfolders_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let base = super::super::py::normpath(&std::fs::canonicalize(&d).unwrap().to_string_lossy().replace(r"\\?\", ""));
        for sub in ["Alice [patreon]", "bob", "Carol [fanbox]/2026 post/sub", "Dave", "dave [fantia]", "Eve [x]/a", "deep/nest/Frank [fanbox]/p"] {
            std::fs::create_dir_all(std::path::Path::new(&base).join("dl").join(sub.replace('/', "\\"))).unwrap();
        }
        for c in fx["folders"].as_array().unwrap() {
            let s = |k: &str| c[k].as_str().unwrap_or("").replace("<B>", &base);
            let name = { let n = s("creator_name"); if n.is_empty() { s("user_id") } else { n } };
            let got = watchlist_folder(&name, &s("service"), &s("artist_dir"), &s("first_target"), &s("download_dir"), c["include_service"].as_bool().unwrap());
            assert_eq!(got, s("folder"), "{:?} {} include={}", c["creator_name"], s("service"), c["include_service"]);
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
