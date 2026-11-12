//! The account favorites: a port of the bridge's fetchFavoriteArtists and of the first half of its
//! favorites download (bridge/app_bridge.py: _start_favorites_download_worker). The creators
//! favorited on the signed-in accounts (Pawchive, cum.st) are fetched, added to the watchlist or
//! marked as favorites there, and turned into the favorites window's cards; then the ones
//! downloaded before are checked for new posts (their first two pages).
//!
//! The app's "watchlist_favorites" call: {"path", "providers": [domain, ...] (the signed-in ones),
//! "mode": "list" (the window's cards, then the check) | "sync" (the creators to download, in
//! order), "tag" (sent back with the first cards), "include_service", "site"}.
//! In "list" mode the cards are sent ahead ("favorite_cards": {"tag", "cards"}) before the check,
//! and the result is the cards with their new posts: {"cards", "checked"}.
//! In "sync" mode the result is {"rids": the creators, in the order found}.

use super::check::Run;
use super::entries;
use crate::downloader::service;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

fn log(level: &str, message: &str) {
    service::log_app(level, message, "watchlist", "");
}

fn is_cumst(domain: &str) -> bool {
    domain.contains("cum.st") || domain.contains("cumst")
}

/// One favorited creator, as the site gave it
struct Fav {
    uid: String,
    svc: String,
    name: String,
    avatar: String,
    domain: String,
    url: String,
}

fn fav_of(item: &Value, provider_domain: &str) -> Fav {
    let s = |k: &str| item.get(k).filter(|v| !v.is_null()).map(entries::text).unwrap_or_default();
    let uid = s("id").trim().to_string();
    let svc = s("service").trim().to_string();
    let name = Some(s("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| uid.clone());
    let domain = Some(s("domain")).filter(|d| !d.is_empty()).unwrap_or_else(|| {
        if provider_domain.is_empty() { "pawchive.pw".into() } else { provider_domain.to_string() }
    });
    let url = if is_cumst(&domain) { format!("https://cum.st/creators/{svc}/{uid}") } else { format!("https://{domain}/{svc}/user/{uid}") };
    Fav { uid, svc, name, avatar: s("avatar"), domain, url }
}

fn card(f: &Fav, e: Option<&entries::Rec>) -> Value {
    let last = e.map(|e| e.s("last_post_date")).unwrap_or_default();
    let new = e.and_then(|e| e.f.get("new_post_count")).and_then(Value::as_u64).unwrap_or(0);
    json!({
        "id": f.uid, "userId": f.uid, "service": f.svc, "name": f.name, "avatar": f.avatar, "url": f.url,
        "domain": f.domain, "provider": if is_cumst(&f.domain) { "cumst" } else { "pawchive" },
        "lastPostDate": last, "status": if last.is_empty() { "Not downloaded" } else { "Synced" },
        "newPostCount": new, "isFavorite": true,
    })
}

pub fn call(id: u64, a: &Value) -> Result<Value, String> {
    let path = a["path"].as_str().ok_or("no path")?.to_string();
    if entries::change(&path, "favorites", |_| ()).is_none() {
        return Err("watchlist not loaded".into());
    }
    let run = Run {
        path: path.clone(),
        id,
        single: true,
        include_service: a["include_service"].as_bool().unwrap_or(true),
        site_args: a["site"].clone(),
        cancel: service::cancel_flag(id),
    };
    let sync = a["mode"].as_str() == Some("sync");
    let client = service::site_client(&run.site_args);

    // The favorites of each signed-in account
    let mut found: Vec<Fav> = Vec::new();
    for domain in a["providers"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if run.cancelled() {
            break;
        }
        for item in client.favorites(domain, "artist", 1, 999_999, 50, &|_, _, _| {}, &run.cancel) {
            found.push(fav_of(&item, domain));
        }
    }
    if run.cancelled() {
        return Ok(json!({"cards": [], "rids": [], "cancelled": true}));
    }
    if found.is_empty() {
        return Ok(json!({"cards": [], "rids": [], "checked": false}));
    }

    // Each one added to the watchlist or marked as a favorite there
    let rids: Vec<i64> = entries::change(&path, "favorites", |w| {
        found.iter().map(|f| w.add_or_update_favorite(&f.uid, &f.svc, &f.name, &f.domain, &f.url, true)).collect()
    })
    .unwrap_or_default();
    let mut order: Vec<i64> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    for r in &rids {
        if seen.insert(*r) {
            order.push(*r);
        }
    }
    if sync {
        log("info", &format!("⭐ Favorite Mode: Found {} favorited creator(s). Starting incremental sync...", found.len()));
        return Ok(json!({"rids": order}));
    }

    // The window's cards (one per creator), sent ahead of the check
    let mut cards: Vec<(i64, Value)> = Vec::new();
    let mut carded: HashSet<(String, String)> = HashSet::new();
    for (f, rid) in found.iter().zip(&rids) {
        if carded.insert((f.uid.clone(), f.svc.clone())) {
            cards.push((*rid, card(f, entries::snapshot(&path, *rid).as_ref())));
        }
    }
    let list = |cards: &[(i64, Value)]| Value::Array(cards.iter().map(|(_, c)| c.clone()).collect());
    service::note_app("favorite_cards", json!({"tag": a["tag"], "cards": list(&cards)}));

    // The ones downloaded before: their new posts (the first two pages)
    let to_check: Vec<i64> = order.iter().copied().filter(|r| entries::snapshot(&path, *r).is_some_and(|e| !e.s("last_post_date").is_empty())).collect();
    if to_check.is_empty() || run.cancelled() {
        return Ok(json!({"cards": list(&cards), "checked": false}));
    }
    let queue = Mutex::new(to_check.clone());
    let counts: Mutex<HashMap<i64, usize>> = Mutex::new(HashMap::new());
    std::thread::scope(|sc| {
        for _ in 0..4.min(to_check.len()) {
            sc.spawn(|| {
                let c = service::site_client(&run.site_args);
                loop {
                    if run.cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let Some(rid) = queue.lock().unwrap().pop() else { return };
                    if let Ok(posts) = run.posts_since(&c, rid, 1, 2, false) {
                        counts.lock().unwrap().insert(rid, posts.len());
                    }
                }
            });
        }
    });
    let counts = counts.into_inner().unwrap();
    for (rid, c) in cards.iter_mut() {
        if let Some(n) = counts.get(rid) {
            c["newPostCount"] = json!(n);
        }
    }
    Ok(json!({"cards": list(&cards), "checked": true}))
}
