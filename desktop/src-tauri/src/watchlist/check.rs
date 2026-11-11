//! Checking the watchlist for new posts: a port of core/watchlist_checker.py (the round: sites in
//! parallel, most recently active creators first, continuing a stopped round, skipping creators a
//! site's list says haven't posted, with spot checks) and of the bridge's check of one creator
//! (bridge/app_bridge.py: _check_watchlist_entry, checkWatchlistArtist): its name put right, its
//! new posts, and the ones whose files are all downloaded already left out.
//!
//! The app's "watchlist_check" call: {"path", "rids" (the creators to check, in order), "single"
//! (one creator, from its button), "include_service", "site": {cookie, user_agent, proxy}}.
//! Progress comes back as [done, total]; each creator's result is saved and the app told about
//! it ("watchlist_changed") as soon as it's known.

use super::entries::{self, Rec};
use crate::downloader::{links, service, sites::SiteClient};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const ROUND_MAX_AGE: f64 = 24.0 * 3600.0;
const UNCHANGED_MARGIN: f64 = 24.0 * 3600.0;
const FULL_CHECK_EVERY: f64 = 7.0 * 24.0 * 3600.0;
const LISTING_MIN_CREATORS: usize = 30;
const SPOT_CHECKS_MIN: usize = 3;
const SPOT_CHECKS_MAX: usize = 100;

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn log(level: &str, message: &str) {
    service::log_app(level, message, "watchlist", "");
}

fn repr(s: &str) -> String {
    crate::downloader::sites::py_repr(s)
}

fn num(e: &Rec, k: &str) -> f64 {
    e.f.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn new_count(e: &Rec) -> usize {
    e.f.get("new_post_count").and_then(Value::as_u64).unwrap_or(0) as usize
}

/// site_of: the creator's site (its domain, else its link's host)
fn site_of(e: &Rec) -> String {
    let d = e.s("domain").trim().to_lowercase();
    if !d.is_empty() {
        return d;
    }
    let url = e.s("url");
    let rest = url.rsplit("://").next().unwrap_or("");
    rest.split('/').next().unwrap_or("").to_lowercase()
}

/// k of the items, picked at random (random.sample)
fn random_sample(items: &[Rec], k: usize) -> Vec<Rec> {
    let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    let mut idx: Vec<usize> = (0..items.len()).collect();
    for i in 0..k.min(idx.len()) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let j = i + (x % (idx.len() - i) as u64) as usize;
        idx.swap(i, j);
    }
    idx.into_iter().take(k).map(|i| items[i].clone()).collect()
}

fn activity_day(e: &Rec) -> String {
    e.s("last_post_date").chars().take(10).collect()
}

/// One check: what it reads and changes
struct Run {
    path: String,
    id: u64,
    single: bool,
    include_service: bool,
    site_args: Value,
    cancel: Arc<AtomicBool>,
}

/// Looking for a creator's files on disk / in the archive: one creator at a time (as in the app)
fn resolve_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(Default::default)
}

fn set(path: &str, rid: i64, fields: Value) {
    let f: Map<String, Value> = fields.as_object().cloned().unwrap_or_default();
    entries::change(path, "check", |w| {
        if let Some(i) = w.index_of(rid) {
            w.set_fields(i, f);
        }
    });
}

impl Run {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn progress(&self, r: &Mutex<Counts>) {
        let c = r.lock().unwrap();
        service::progress_app(self.id, c.done, c.total);
    }

    /// _check_watchlist_entry (with get_posts_since's name fix-up): the creator's new posts.
    /// Err when its post list couldn't be fetched (the updates found earlier are kept).
    fn check_one(&self, c: &SiteClient, rid: i64) -> Result<usize, String> {
        let mut new = self.posts_since(c, rid, 1, 999_999, true)?;
        let name = entries::snapshot(&self.path, rid).map(|e| e.s("creator_name")).unwrap_or_default();
        if !new.is_empty() {
            let _one = resolve_lock().lock().unwrap_or_else(|e| e.into_inner());
            new = self.already_downloaded(rid, new);
        }
        set(&self.path, rid, json!({"new_post_count": new.len()}));
        if !new.is_empty() {
            let name = entries::snapshot(&self.path, rid).map(|e| e.s("creator_name")).unwrap_or(name);
            log("info", &format!("Watchlist: {} has {} new post(s).", repr(&name), new.len()));
        }
        Ok(new.len())
    }

    /// get_posts_since: the creator's name put right, then its posts after the last download
    /// (kept as its updates found). Err when a page couldn't be fetched and `raise_on_error`.
    fn posts_since(&self, c: &SiteClient, rid: i64, page_start: usize, page_end: usize, raise_on_error: bool) -> Result<Vec<Value>, String> {
        let Some(e) = entries::snapshot(&self.path, rid) else { return Ok(Vec::new()) };
        let url = e.s("url");
        let parsed = links::parse(&url);
        if !parsed.is_valid {
            log("warning", &format!("Watchlist: could not parse URL for {}: {url}", repr(&e.s("creator_name"))));
            return Ok(Vec::new());
        }
        // The name, when it's missing, the creator's id, or another creator's of the same site
        let (name, uid, svc) = (e.s("creator_name"), e.s("user_id"), e.s("service"));
        let dup = !name.is_empty()
            && entries::all_of(&self.path).iter().any(|o| o.rid != rid && o.s("service").to_lowercase() == svc.to_lowercase() && !o.s("creator_name").is_empty() && o.s("creator_name").to_lowercase() == name.to_lowercase());
        if name == uid || name.is_empty() || dup {
            if let Some(resolved) = c.creator_name(&parsed).filter(|r| !r.is_empty() && *r != uid && *r != name) {
                let (u, d, inc) = (url.clone(), e.s("domain"), self.include_service);
                entries::change(&self.path, "check", |w| w.add_entry(&u, &resolved, &uid, &svc, &d, "", "", true, "", &json!({}), false, inc));
                log("info", &format!("Auto-healed creator name for [{svc}] {uid}: {} -> {}", repr(&name), repr(&resolved)));
            }
        }
        let e = entries::snapshot(&self.path, rid).unwrap_or(e);
        let fields = Value::Object(e.f.clone());
        let entry = super::Entry::from_value(&fields);
        let cancel = self.cancel.clone();
        let mut fetch = |page: usize| {
            let posts = c.user_posts(&parsed, page, page, 50, &|_, _, _| {}, &cancel, "", "");
            (posts, c.last_fetch_failed.get())
        };
        let r = super::posts_since(&entry, &mut fetch, page_start, page_end, raise_on_error, &|| self.cancelled(), &|level: &str, msg: &str| log(&level.to_lowercase(), msg));
        if let Some(p) = r.failed_page {
            return Err(format!("page {p} of the post list couldn't be fetched"));
        }
        set(&self.path, rid, json!({"cached_new_posts": r.posts, "new_post_count": r.posts.len()}));
        Ok(r.posts)
    }

    /// _check_and_resolve_if_already_downloaded: the posts with something left to download (the
    /// "downloaded up to" point moves past the others)
    fn already_downloaded(&self, rid: i64, posts: Vec<Value>) -> Vec<Value> {
        let Some(e) = entries::snapshot(&self.path, rid) else { return posts };
        let (uid, svc) = (e.s("user_id"), e.s("service"));
        // (the creator's download settings and folder, from the app)
        let args = match service::ask_app("watch_plan_args", json!([uid, svc])) {
            Ok(a) if a.is_object() => a,
            Ok(_) => return posts,
            Err(err) => {
                log("debug", &format!("Watchlist check pre-flight verification error: {err}"));
                return posts;
            }
        };
        let plan = service::plan_build(self.id, &args, &posts, Some(self.cancel.clone()));
        if plan.cancelled {
            return posts; // (stopped: no files doesn't mean they're all downloaded)
        }
        let id_of = |p: &Value| super::entries::text(p.get("id").unwrap_or(&json!("")));
        if plan.tasks.is_empty() {
            // Everything's downloaded (or filtered out)
            let latest = posts.last().unwrap();
            let (pid, day) = (id_of(latest), super::post_day(latest));
            let day_ids: Vec<String> = posts.iter().filter(|p| super::post_day(p) == day).map(id_of).collect();
            entries::change(&self.path, "check", |w| {
                w.resolve_posts(&uid, &svc, None, &pid, &day, &day_ids);
                if let Some(i) = w.index_of(rid) {
                    w.set_fields(i, json!({"new_post_count": 0, "cached_new_posts": []}).as_object().cloned().unwrap());
                }
            });
            return Vec::new();
        }
        let with_work: HashSet<String> = plan.tasks.iter().map(|t| t.post_id.clone()).collect();
        let remaining: Vec<Value> = posts.iter().filter(|p| with_work.contains(&id_of(p))).cloned().collect();
        if !remaining.is_empty() && remaining.len() < posts.len() {
            let done: Vec<String> = posts.iter().map(id_of).filter(|i| !with_work.contains(i)).collect();
            let keep = remaining.clone();
            entries::change(&self.path, "check", |w| {
                w.resolve_posts(&uid, &svc, Some(&done), "", "", &[]);
                if let Some(i) = w.index_of(rid) {
                    w.set_fields(i, json!({"cached_new_posts": keep, "new_post_count": keep.len()}).as_object().cloned().unwrap());
                }
            });
            return remaining;
        }
        posts
    }

    /// WatchlistChecker._check: one creator, its result saved; None when it couldn't be checked
    fn check(&self, c: &SiteClient, rid: i64, counts: &Mutex<Counts>) -> Option<usize> {
        let n = match self.check_one(c, rid) {
            Ok(n) => Some(n),
            Err(ex) => {
                counts.lock().unwrap().failed += 1;
                let name = entries::snapshot(&self.path, rid).map(|e| e.s("creator_name")).unwrap_or_else(|| "?".into());
                log("warning", &format!("Watchlist check error for {}: {ex}", repr(&name)));
                None
            }
        };
        if let Some(n) = n {
            set(&self.path, rid, json!({"new_post_count": n, "last_checked_at": now()}));
            counts.lock().unwrap().total_new += n;
        }
        counts.lock().unwrap().done += 1;
        self.progress(counts);
        n
    }
}

#[derive(Default)]
struct Counts {
    done: usize,
    total: usize,
    total_new: usize,
    skipped_unchanged: usize,
    failed: usize,
}

/// The creators of a site that its list says haven't posted since their last check
fn unchanged(run: &Run, c: &SiteClient, site: &str, items: &[Rec], at: f64) -> HashSet<i64> {
    if items.len() < LISTING_MIN_CREATORS {
        return HashSet::new();
    }
    let candidates: Vec<&Rec> = items.iter().filter(|e| num(e, "last_checked_at") > 0.0 && at - num(e, "last_checked_at") < FULL_CHECK_EVERY).collect();
    let cost = c.creator_listing_cost(site);
    if candidates.len() < LISTING_MIN_CREATORS.max((cost as f64 * 1.5) as usize) {
        return HashSet::new();
    }
    let Some(updated) = c.creator_updates(site, 600.0, &run.cancel).filter(|m| !m.is_empty()) else {
        return HashSet::new();
    };
    let mut skip = HashSet::new();
    for e in candidates {
        let svc = e.s("service").to_lowercase();
        let uid = e.s("user_id");
        let u = updated.get(&(svc.clone(), uid.clone())).or_else(|| updated.get(&(svc, uid.to_lowercase())));
        if let Some(&u) = u {
            if u != 0.0 && u < num(e, "last_checked_at") - UNCHANGED_MARGIN {
                skip.insert(e.rid);
            }
        }
    }
    if !skip.is_empty() {
        log("info", &format!("Watchlist: {} of {} {site} artist(s) haven't posted since their last check; skipped.", skip.len(), items.len()));
    }
    skip
}

fn round_start(path: &str, resume: bool) -> f64 {
    let store = super::store::store(path);
    let t = now();
    let r = (|| -> Result<f64, String> {
        let started: f64 = store.get_meta("check_round_started", "0")?.parse().unwrap_or(0.0);
        let finished = store.get_meta("check_round_finished", "")? == py_float(started);
        if resume && started != 0.0 && !finished && t - started < ROUND_MAX_AGE {
            return Ok(started);
        }
        store.set_meta("check_round_started", &py_float(t))?;
        Ok(t)
    })();
    r.unwrap_or_else(|e| {
        log("debug", &format!("Watchlist check round not saved: {e}"));
        t
    })
}

/// str(x) of a float (a time: Python's shortest form, "1.0" for a whole number)
fn py_float(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e16 { format!("{x:.1}") } else { format!("{x}") }
}

/// WatchlistChecker.run: checks the creators; the number of new posts found
fn run_round(run: &Run, rids: &[i64], resume: bool) -> Counts {
    let entries: Vec<Rec> = rids.iter().filter_map(|r| entries::snapshot(&run.path, *r)).collect();
    let started = round_start(&run.path, resume);
    let mut counts = Counts::default();
    let todo: Vec<Rec> = entries.iter().filter(|e| num(e, "last_checked_at") < started).cloned().collect();
    let already = entries.len() - todo.len();
    if already > 0 {
        log("info", &format!("Watchlist: continuing the last check ({already} of {} artist(s) already done).", entries.len()));
        counts.total_new += entries.iter().filter(|e| num(e, "last_checked_at") >= started).map(new_count).sum::<usize>();
    }
    let mut by_site: Vec<(String, Vec<Rec>)> = Vec::new();
    for e in todo {
        let site = site_of(&e);
        match by_site.iter_mut().find(|(s, _)| *s == site) {
            Some((_, v)) => v.push(e),
            None => by_site.push((site, vec![e])),
        }
    }
    let at = now();
    counts.total = entries.len();
    counts.done = already;
    let counts = Mutex::new(counts);
    run.progress(&counts);

    let client = service::site_client(&run.site_args);
    let mut queues: HashMap<String, VecDeque<i64>> = HashMap::new();
    let mut held: HashMap<String, Vec<Rec>> = HashMap::new();
    let mut spots: HashMap<String, HashMap<i64, usize>> = HashMap::new();
    for (site, mut items) in by_site.clone() {
        items.sort_by(|a, b| activity_day(b).cmp(&activity_day(a)));
        let mut skip = unchanged(run, &client, &site, &items, at);
        let mut sample: Vec<Rec> = Vec::new();
        if !skip.is_empty() {
            let skipped: Vec<Rec> = items.iter().filter(|e| skip.contains(&e.rid)).cloned().collect();
            let k = skipped.len().min(SPOT_CHECKS_MAX).min(SPOT_CHECKS_MIN.max(skipped.len() / 50));
            sample = random_sample(&skipped, k);
            for e in &sample {
                skip.remove(&e.rid);
            }
            held.insert(site.clone(), skipped.into_iter().filter(|e| skip.contains(&e.rid)).collect());
            spots.insert(site.clone(), sample.iter().map(|e| (e.rid, new_count(e))).collect());
        }
        let ids: HashSet<i64> = sample.iter().map(|e| e.rid).collect();
        let q: VecDeque<i64> = sample.iter().map(|e| e.rid).chain(items.iter().filter(|e| !skip.contains(&e.rid) && !ids.contains(&e.rid)).map(|e| e.rid)).collect();
        queues.insert(site, q);
    }
    let mut sites: Vec<String> = by_site.iter().map(|(s, _)| s.clone()).collect();
    sites.sort_by_key(|s| std::cmp::Reverse(queues[s].len()));
    let pending = Mutex::new(sites.clone());
    let queues = Mutex::new(queues);
    let held = Mutex::new(held);
    let workers = 4.min(sites.len()).max(1);
    std::thread::scope(|sc| {
        for _ in 0..workers {
            sc.spawn(|| {
                let c = service::site_client(&run.site_args);
                while !run.cancelled() {
                    let Some(site) = ({
                        let mut p = pending.lock().unwrap();
                        if p.is_empty() { None } else { Some(p.remove(0)) }
                    }) else {
                        return;
                    };
                    let mut waiting = spots.get(&site).cloned().unwrap_or_default();
                    let mut doubt = false;
                    loop {
                        if run.cancelled() {
                            return;
                        }
                        let Some(rid) = queues.lock().unwrap().get_mut(&site).and_then(VecDeque::pop_front) else { break };
                        let n = run.check(&c, rid, &counts);
                        if let Some(before) = waiting.remove(&rid) {
                            if n.is_some_and(|n| n > before) {
                                doubt = true;
                            }
                            if waiting.is_empty() {
                                if let Some(skipped) = held.lock().unwrap().remove(&site) {
                                    if doubt {
                                        log("warning", &format!("Watchlist: {site}'s creator list missed new posts in a spot check; checking all {} artist(s) it would have skipped.", skipped.len()));
                                        queues.lock().unwrap().get_mut(&site).unwrap().extend(skipped.iter().map(|e| e.rid));
                                    } else {
                                        let mut k = counts.lock().unwrap();
                                        k.skipped_unchanged += skipped.len();
                                        k.total_new += skipped.iter().map(new_count).sum::<usize>();
                                        k.done += skipped.len();
                                        drop(k);
                                        run.progress(&counts);
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    if !run.cancelled() {
        let _ = super::store::store(&run.path).set_meta("check_round_finished", &py_float(started));
    }
    counts.into_inner().unwrap()
}

/// checkWatchlistArtist's check: the name looked up again, then the creator's new posts
fn run_single(run: &Run, rid: i64) -> Counts {
    let mut counts = Counts { total: 1, ..Default::default() };
    let c = service::site_client(&run.site_args);
    let Some(e) = entries::snapshot(&run.path, rid) else { return counts };
    let url = e.s("url");
    let url = if url.is_empty() { format!("https://{}/{}/user/{}", e.s("domain"), e.s("service"), e.s("user_id")) } else { url };
    let parsed = links::parse(&url);
    if parsed.is_valid {
        let old = e.s("creator_name");
        if let Some(resolved) = c.creator_name(&parsed).filter(|r| !r.is_empty() && *r != e.s("user_id") && *r != old) {
            set(&run.path, rid, json!({"creator_name": resolved}));
            log("info", &format!("Auto-healed creator name in watchlist: {} -> {}", repr(&old), repr(&resolved)));
        }
    }
    match run.check_one(&c, rid) {
        Ok(n) => {
            set(&run.path, rid, json!({"last_checked_at": now()}));
            counts.total_new = n;
        }
        Err(ex) => {
            let name = entries::snapshot(&run.path, rid).map(|e| e.s("creator_name")).unwrap_or_default();
            log("warning", &format!("Watchlist check error for {}: {ex}", repr(&name)));
            counts.failed = 1;
        }
    }
    counts.done = 1;
    counts
}

/// _process_watchlist_entry_download up to queueing: the creator's new posts (those asked for),
/// their folder and plan. {"status": "none"} when there's nothing to download, "done" when it's all
/// downloaded already (the creator is marked up to date), else "plan" with the plan for the app to
/// queue. The app's "watchlist_download" call: {"path", "rid", "post_ids", "page_start",
/// "page_end", "custom_dir", "include_service", "site"}.
pub fn download(id: u64, a: &Value) -> Result<Value, String> {
    let path = a["path"].as_str().ok_or("no path")?.to_string();
    if entries::change(&path, "download", |_| ()).is_none() {
        return Err("watchlist not loaded".into());
    }
    let rid = a["rid"].as_i64().ok_or("no creator")?;
    let run = Run { path: path.clone(), id, single: true, include_service: a["include_service"].as_bool().unwrap_or(true), site_args: a["site"].clone(), cancel: service::cancel_flag(id) };
    let c = service::site_client(&run.site_args);
    let n = |k: &str, d: usize| a[k].as_u64().map(|x| x as usize).unwrap_or(d);
    let (page_start, page_end) = (n("page_start", 1), n("page_end", 999_999));
    let mut posts = run.posts_since(&c, rid, page_start, page_end, false)?;
    let Some(e) = entries::snapshot(&path, rid) else { return Ok(json!({"status": "none"})) };
    let (name, uid, svc) = (e.s("creator_name"), e.s("user_id"), e.s("service"));
    if posts.is_empty() {
        log("info", &format!("No new posts found for {}.", repr(&name)));
        return Ok(json!({"status": "none"}));
    }
    let id_of = |p: &Value| entries::text(p.get("id").unwrap_or(&json!("")));
    let wanted: Option<Vec<String>> = a["post_ids"].as_array().filter(|p| !p.is_empty()).map(|p| p.iter().map(entries::text).collect());
    if let Some(w) = &wanted {
        let set_: HashSet<&String> = w.iter().collect();
        posts.retain(|p| set_.contains(&id_of(p)));
        if posts.is_empty() {
            log("info", &format!("None of the requested posts for {} were available.", repr(&name)));
            let w = w.clone();
            entries::change(&path, "download", |x| x.resolve_posts(&uid, &svc, Some(&w), "", "", &[]));
            return Ok(json!({"status": "none"}));
        }
    }
    let latest = posts.last().unwrap();
    let (latest_pid, latest_day) = (id_of(latest), super::post_day(latest));
    // The creator's settings and folder, from the app
    let args = service::ask_app("watch_plan_args", json!([uid, svc, {"download": true, "custom_dir": a["custom_dir"], "page_start": page_start, "page_end": page_end}]))?;
    let artist = args["request"]["artist_dir"].as_str().unwrap_or("").to_string();
    let custom = a["custom_dir"].as_str().is_some_and(|c| !c.is_empty());
    let a2 = artist.clone();
    entries::change(&path, "download", |w| {
        if let Some(i) = w.index_of(rid) {
            let e = &w.list()[i];
            let mut f = Map::new();
            if e.s("download_dir").is_empty() || custom {
                f.insert("download_dir".into(), json!(a2));
            }
            let mut dirs: Vec<Value> = e.list("download_dirs");
            if !dirs.iter().any(|d| d.as_str() == Some(a2.as_str())) {
                dirs.insert(0, json!(a2));
                f.insert("download_dirs".into(), Value::Array(dirs));
            }
            w.set_fields(i, f);
        }
    });
    let plan = service::plan_build(id, &args, &posts, Some(run.cancel.clone()));
    let post_list = Value::Array(posts.clone());
    if plan.cancelled {
        return Ok(json!({"status": "none", "posts": post_list}));
    }
    if plan.tasks.is_empty() {
        // Everything's already downloaded or archived (or filtered out)
        let day_ids: Vec<String> = posts.iter().filter(|p| super::post_day(p) == latest_day).map(id_of).collect();
        let w2 = wanted.clone();
        entries::change(&path, "download", |x| x.resolve_posts(&uid, &svc, w2.as_deref(), &latest_pid, &latest_day, &day_ids));
        log("info", &if wanted.is_some() {
            format!("Watchlist: selected post(s) for {} are already archived or downloaded. Review drawer updated.", repr(&name))
        } else {
            format!("Watchlist: all {} new post(s) for {} are already archived or downloaded. Marked as up to date.", posts.len(), repr(&name))
        });
        return Ok(json!({"status": "done", "posts": post_list}));
    }
    let no_file: Vec<Value> = {
        let with: HashSet<&String> = plan.tasks.iter().map(|t| &t.post_id).collect();
        posts.iter().filter(|p| !with.contains(&id_of(p))).map(|p| json!([super::post_day(p), id_of(p)])).collect()
    };
    Ok(json!({
        "status": "plan",
        "posts": post_list,
        "artist_dir": artist,
        "no_file_posts": no_file,
        "options": args["options"],
        "plan": {
            "tasks": plan.tasks,
            "pending_post_infos": plan.pending_post_infos,
            "write_now": plan.write_now,
            "harvested_links": plan.harvested_links,
            "harvested_records": plan.harvested_records,
            "not_queued": plan.not_queued,
            "renamed": plan.renamed,
            "unmatched_known": plan.unmatched_known,
            "cancelled": plan.cancelled,
        },
    }))
}

pub fn call(id: u64, a: &Value) -> Result<Value, String> {
    let path = a["path"].as_str().ok_or("no path")?.to_string();
    if entries::change(&path, "check", |_| ()).is_none() {
        return Err("watchlist not loaded".into());       // (the app sends it, then asks again)
    }
    let rids: Vec<i64> = a["rids"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
    let run = Run {
        path,
        id,
        single: a["single"].as_bool().unwrap_or(false),
        include_service: a["include_service"].as_bool().unwrap_or(true),
        site_args: a["site"].clone(),
        cancel: service::cancel_flag(id),
    };
    let c = if run.single { run_single(&run, rids.first().copied().unwrap_or(-1)) } else { run_round(&run, &rids, a["resume"].as_bool().unwrap_or(true)) };
    Ok(json!({"total_new": c.total_new, "failed": c.failed, "skipped_unchanged": c.skipped_unchanged, "done": c.done, "total": c.total, "cancelled": run.cancelled()}))
}
