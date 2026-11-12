//! The Rust download queue, run for the Python app until the bridge itself moves here.
//!
//! A local socket (like the transfer engine's). The app connects once with its token
//! (`{"token": "…"}`) and the line protocol runs both ways, one JSON object per line:
//!
//! From the app:
//!   `{"op": "sync", "tasks": [...], "options": {"o1": {...}}, "replace": true}`  the queue as it
//!   is now (with "replace", the app's statuses win: sent before a run starts)
//!   `{"op": "start", "options": {...}, "workers": 4}`
//!   `{"op": "pause"}`, `resume`, `cancel`, `reset`, `{"op": "workers", "n": 4}`
//!   `{"op": "state", "key", "status", "error_msg", "retry_count"}`  a file's status changed there
//!   `{"op": "batch", "batch", "paused": true}`
//!   `{"op": "view", "order", "rows", "meta"}`  the window's queue list changed (queue_view.rs)
//!   `{"op": "watchlist_view", "order", "rows", "meta", "full"}`  the same for the watchlist
//!   `{"t": "reply", "id", "value"}`  the answer to a question below
//!   `{"op": "call", "id", "fn", "args"}`  work done here for the app, answered with
//!   `{"t": "result", "id", "value"}` (or `"error"`): fn "plan" builds the download tasks of
//!   scanned posts (args: posts, options, request); fn "site" is the archive sites' client
//!   (post lists, DMs, single posts, profiles, tags, favorites, creator lists), cancelled with
//!   `{"op": "cancel_call", "id"}`
//!
//! To the app:
//!   `{"t": "task", "key", ...state}`  a file changed (status, progress, where it's saved)
//!   `{"t": "progress", "info"}`, `{"t": "finished", "ok", "summary"}`, `{"t": "workers", "n"}`
//!   `{"t": "log", "level", "message"}`, `{"t": "checkpoint"}`, `{"t": "discard_recovery"}`
//!   `{"t": "record_file", "key", "size", "path"}`
//!   `{"t": "ask", "id", "fn", "args"}` for what the app still knows or does: is_archived, cookie,
//!   refresh_link, special (yt-dlp / Telegram files), limit; while planning also
//!   archived, plan_cancelled
//!   `{"t": "note", "fn", "args"}`  the same without an answer: record_link, archive_password,
//!   add_passwords, log, queue_action (the window paused / resumed / cancelled / removed a post or a
//!   file of the queue here: queue_window)
//!
//! The queue's messages are handled in the order sent; calls each run on their own thread.

use super::fetch::{Cell, Ctl, Hooks, State};
use super::filters::Options;
use super::plan::{self, Lookups, Task};
use super::links::ParsedLink;
use super::sites::SiteClient;
use super::queue::{Queue, QueueHooks};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};

pub struct ServiceInfo {
    pub url: String,
    pub token: String,
}

/// The connection to the app (None when it isn't connected)
#[derive(Default)]
struct Link {
    out: Mutex<Option<TcpStream>>,
    asks: Mutex<HashMap<u64, Sender<Value>>>,
    next: AtomicU64,
}

impl Link {
    fn send(&self, v: Value) -> bool {
        let mut g = self.out.lock().unwrap();
        let Some(s) = g.as_mut() else { return false };
        let mut b = v.to_string().into_bytes();
        b.push(b'\n');
        if s.write_all(&b).and_then(|_| s.flush()).is_err() {
            *g = None;
            return false;
        }
        true
    }

    /// Asks the app and waits for its answer (Null when it's gone)
    fn ask(&self, f: &str, args: Value) -> Value {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.asks.lock().unwrap().insert(id, tx);
        if !self.send(json!({"t": "ask", "id": id, "fn": f, "args": args})) {
            self.asks.lock().unwrap().remove(&id);
            return Value::Null;
        }
        // yt-dlp / Telegram files take as long as they take; the rest answers at once
        let answer = rx.recv().unwrap_or(Value::Null);
        self.asks.lock().unwrap().remove(&id);
        answer
    }

    /// Tells the app something that needs no answer
    fn note(&self, f: &str, args: Value) {
        self.send(json!({"t": "note", "fn": f, "args": args}));
    }

    fn reply(&self, id: u64, value: Value) {
        if let Some(tx) = self.asks.lock().unwrap().remove(&id) {
            let _ = tx.send(value);
        }
    }

    /// The connection ended: every waiting question gets no answer
    fn drop_all(&self) {
        *self.out.lock().unwrap() = None;
        for (_, tx) in self.asks.lock().unwrap().drain() {
            let _ = tx.send(Value::Null);
        }
    }
}

struct Remote(Arc<Link>);

fn task_event(c: &Cell) -> Value {
    let it = c.lock().unwrap();
    let mut v = serde_json::to_value(&it.st).unwrap_or(json!({}));
    let m = v.as_object_mut().unwrap();
    m.insert("t".into(), json!("task"));
    m.insert("key".into(), json!(it.task.key));
    m.insert("url".into(), json!(it.task.url));
    m.insert("target_path".into(), json!(it.task.target_path));
    m.insert("file_size".into(), json!(it.task.file_size));
    m.insert("fallback_urls".into(), json!(it.task.fallback_urls));
    v
}

impl Hooks for Remote {
    fn refresh_temporary_link(&self, t: &Task) -> Option<String> {
        self.0.ask("refresh_link", json!([t.key])).as_str().filter(|s| !s.is_empty()).map(str::to_string)
    }
    fn cookie_for(&self, url: &str) -> String {
        self.0.ask("cookie", json!([url])).as_str().unwrap_or("").to_string()
    }
    fn special(&self, c: &Cell, _o: &Options, _ctl: &Ctl) -> (bool, String) {
        let key = c.lock().unwrap().task.key.clone();
        let a = self.0.ask("special", json!([key]));
        match (a.get(0).and_then(Value::as_bool), a.get(1).and_then(Value::as_str)) {
            (Some(ok), Some(msg)) => (ok, msg.to_string()),
            _ => (false, "Cancelled".into()),
        }
    }
    fn ytdlp(&self) -> Option<(String, String)> {
        let a = self.0.ask("ytdlp", json!([]));
        let exe = a.get(0).and_then(Value::as_str).filter(|s| !s.is_empty())?;
        Some((exe.to_string(), a.get(1).and_then(Value::as_str).unwrap_or("").to_string()))
    }
    fn changed(&self, c: &Cell) {
        view_changed();
        self.0.send(task_event(c));
    }
    fn limit_bps(&self) -> f64 {
        self.0.ask("limit", json!([])).as_f64().unwrap_or(0.0)
    }
}

impl QueueHooks for Remote {
    fn progress(&self, info: &Value) {
        self.0.send(json!({"t": "progress", "info": info}));
    }
    fn finished(&self, ok: bool, summary: &str) {
        self.0.send(json!({"t": "finished", "ok": ok, "summary": summary}));
    }
    fn concurrency(&self, n: usize) {
        self.0.send(json!({"t": "workers", "n": n}));
    }
    fn many_changed(&self, cells: &[Cell]) {
        for c in cells {
            self.changed(c);
        }
    }
    fn checkpoint(&self) {
        self.0.send(json!({"t": "checkpoint"}));
    }
    fn discard_recovery(&self) {
        self.0.send(json!({"t": "discard_recovery"}));
    }
    fn is_cloud_link(&self, url: &str) -> bool {
        cloud_link(url)
    }
    fn log(&self, level: &str, message: &str) {
        self.0.send(json!({"t": "log", "level": level, "message": message}));
    }
}

/// What the planner looks up in the app while it plans one call's posts
struct PlanCtx {
    link: Arc<Link>,
    id: u64,
    sites: SiteClient,
    /// Stopping (a watchlist check's own flag), instead of asking the app
    stop: Option<Arc<AtomicBool>>,
}

impl Lookups for PlanCtx {
    fn resolve_temporary_links(&self, post: &mut Value, domain: &str) {
        self.sites.resolve_temporary_links(post, domain);
    }
    fn record_archive_password(&self, archive_path: &str, password: &str) {
        self.link.note("archive_password", json!([archive_path, password]));
    }
    fn add_passwords(&self, passwords: &[String]) {
        self.link.note("add_passwords", json!([passwords]));
    }
    fn cancelled(&self) -> bool {
        if let Some(stop) = &self.stop {
            return stop.load(Ordering::Relaxed);
        }
        self.link.ask("plan_cancelled", json!([self.id])).as_bool().unwrap_or(true)
    }
    fn log(&self, level: &str, message: &str, to_file: bool) {
        self.link.note("log", json!([level, message, to_file]));
    }
}

fn plan_call(id: u64, args: &Value) -> Result<Value, String> {
    let posts = args["posts"].as_array().ok_or("no posts")?;
    let p = plan_build(id, args, posts, None);
    Ok(json!({
        "tasks": p.tasks,
        "pending_post_infos": p.pending_post_infos,
        "write_now": p.write_now,
        "harvested_links": p.harvested_links,
        "harvested_records": p.harvested_records,
        "not_queued": p.not_queued,
        "renamed": p.renamed,
        "cancelled": p.cancelled,
    }))
}

/// The planner on these posts, with the plan call's arguments (options, request)
pub fn plan_build(id: u64, args: &Value, posts: &[Value], stop: Option<Arc<AtomicBool>>) -> plan::Plan {
    let o = Options::from_dict(&args["options"]);
    let r = &args["request"];
    let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("");
    let opt = |k: &str| r.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    let req = plan::Request {
        creator_name: s("creator_name"),
        service: s("service"),
        domain: s("domain"),
        base_dir: s("base_dir"),
        batch_id: opt("batch_id"),
        artist_dir: opt("artist_dir"),
        user_id: s("user_id"),
        include_service: r.get("include_service").and_then(Value::as_bool).unwrap_or(true),
    };
    let l = link().clone();
    let cookies = l.clone();
    let mut sites = SiteClient::new(
        args.get("cookie").and_then(Value::as_str).unwrap_or(""),
        Box::new(move |site: &str| cookies.ask("account_cookie", json!([site])).as_str().unwrap_or("").to_string()),
    );
    let lg = l.clone();
    sites.set_log(Box::new(move |level: &str, category: &str, msg: &str| lg.note("log", json!([level, msg, true, category]))));
    let ctx = PlanCtx { link: l, id, sites, stop };
    plan::build(posts, &o, &req, &ctx)
}

/// Cancel flags of the calls running now ({"op": "cancel_call", "id"} sets one)
fn call_flags() -> &'static Mutex<HashMap<u64, Arc<AtomicBool>>> {
    static F: OnceLock<Mutex<HashMap<u64, Arc<AtomicBool>>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

pub fn parsed_link(v: &Value) -> ParsedLink {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    ParsedLink {
        domain: s("domain"),
        service: s("service"),
        user_id: s("user_id"),
        post_id: v.get("post_id").and_then(Value::as_str).filter(|p| !p.is_empty()).map(str::to_string),
        provider: s("provider"),
        is_single_post: b("is_single_post"),
        is_dms: b("is_dms"),
        is_valid: v.get("is_valid").and_then(Value::as_bool).unwrap_or(true),
        raw_url: s("raw_url"),
        error_msg: s("error_msg"),
        extra_data: v.get("extra_data").and_then(Value::as_object).cloned().unwrap_or_default(),
    }
}

/// The archive sites' client, for the app's scans: args {"fn", "cookie", "user_agent", "proxy", ...}.
/// Log lines and progress ({"t": "note", "fn": "progress", "args": [id, page, count, total]})
/// come back while it runs.
/// The archive sites' client the app asked for ({"cookie", "user_agent", "proxy"}), logging to the app
pub fn site_client(a: &Value) -> SiteClient {
    let l = link().clone();
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let cookies = l.clone();
    let mut c = SiteClient::with_proxy(
        &s("cookie"),
        Box::new(move |site: &str| cookies.ask("account_cookie", json!([site])).as_str().unwrap_or("").to_string()),
        &s("proxy"),
    );
    if !s("user_agent").trim().is_empty() {
        c.user_agent = s("user_agent").trim().to_string();
    }
    c.set_log(Box::new(move |level: &str, category: &str, msg: &str| l.note("log", json!([level, msg, true, category]))));
    c
}

/// A call's cancel flag (set by {"op": "cancel_call"})
pub fn cancel_flag(id: u64) -> Arc<AtomicBool> {
    call_flags().lock().unwrap().entry(id).or_default().clone()
}

/// A scan's progress through the post pages (the app's call() hands it to the caller)
pub fn progress_pages(id: u64, page: usize, count: usize, total: Option<u64>) {
    link().note("progress", json!([id, page, count, total]));
}

/// A call's progress as one value (the app's call() hands it to the caller)
pub fn progress_value(id: u64, v: Value) {
    link().note("progress", json!([id, v]));
}

/// Progress of a call: [id, a, b, c] (the app's call() hands it to the caller)
pub fn progress_app(id: u64, a: usize, b: usize) {
    link().note("progress", json!([id, a, b]));
}

fn site_call(id: u64, a: &Value) -> Result<Value, String> {
    let l = link().clone();
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let n = |k: &str, d: usize| a.get(k).and_then(Value::as_u64).map(|x| x as usize).unwrap_or(d);
    let c = site_client(a);
    let cancel = cancel_flag(id);
    let progress = |page: usize, count: usize, total: Option<u64>| l.note("progress", json!([id, page, count, total]));
    let p = parsed_link(&a["link"]);
    let out = match s("fn").as_str() {
        "posts" => {
            let posts = c.user_posts(&p, n("page_start", 1), n("page_end", 999_999), n("page_size", 50), &progress, &cancel, &s("date_after"), &s("date_before"));
            json!({"posts": posts, "failed": c.last_fetch_failed.get()})
        }
        "dms" => json!(c.user_dms(&p, n("page_start", 1), n("page_end", 999_999), n("page_size", 50), &progress, &cancel)),
        "single_post" => json!(c.single_post(&p)),
        "comments" => json!(c.post_comments(&s("domain"), &s("service"), &s("user_id"), &s("post_id"))),
        "profile" => c.creator_profile(&p),
        "creator_name" => json!(c.creator_name(&p)),
        "tags" => json!(c.creator_tags(&p)),
        "favorites" => json!(c.favorites(&s("domain"), &s("fav_type"), n("page_start", 1), n("page_end", 999_999), n("page_size", 50), &progress, &cancel)),
        "listing_cost" => json!(c.creator_listing_cost(&s("domain"))),
        "creator_updates" => {
            let max_age = a.get("max_age").and_then(Value::as_f64).unwrap_or(600.0);
            match c.creator_updates(&s("domain"), max_age, &cancel) {
                Some(m) => json!(m.into_iter().map(|((svc, uid), t)| json!([svc, uid, t])).collect::<Vec<_>>()),
                None => Value::Null,
            }
        }
        "posts_since" => {
            // The watchlist's check for new posts (src/watchlist/): one page at a time
            let entry = crate::watchlist::Entry::from_value(&a["entry"]);
            let mut fetch = |page: usize| {
                let posts = c.user_posts(&p, page, page, 50, &|_, _, _| {}, &cancel, "", "");
                (posts, c.last_fetch_failed.get())
            };
            let wl = l.clone();
            let r = crate::watchlist::posts_since(
                &entry,
                &mut fetch,
                n("page_start", 1),
                n("page_end", 999_999),
                a["raise_on_error"].as_bool().unwrap_or(false),
                &|| cancel.load(std::sync::atomic::Ordering::Relaxed),
                &move |level: &str, msg: &str| wl.note("log", json!([level, msg, true, "watchlist"])),
            );
            json!({"posts": r.posts, "failed_page": r.failed_page})
        }
        "resolve_links" => {
            let mut post = a["post"].clone();
            let k = c.resolve_temporary_links(&mut post, &s("domain"));
            json!({"post": post, "resolved": k})
        }
        other => return Err(format!("unknown site call {other}")),
    };
    Ok(out)
}

/// Tells the app something (a note: no answer)
/// The window's queue actions on a post or a file (pause, resume, cancel, remove), done here on the
/// engine's own copy of the queue; the app is told ("queue_action": [method, post or file, {"n",
/// "start"}]) to follow (its paused posts, removing the files, starting a run when files are
/// waiting and nothing runs). None when the engine doesn't have those files (the app does it).
pub fn queue_window(method: &str, args: &[Value]) -> Option<Value> {
    let _touch = Touch;
    // (the app's own list: the old Python downloader, the engine's queue isn't used)
    if link().out.lock().unwrap().is_none() || super::queue_view::global().lock().unwrap().from_app {
        return None;
    }
    let q = queue();
    let id = args.first().and_then(Value::as_str).unwrap_or("").to_string();
    let method = match method {
        "toggleBatchPause" if q.has_batch(&id) => if q.is_batch_paused(&id) { "resumeBatch" } else { "pauseBatch" },
        m => m,
    };
    let skip_404 = super::queue_view::global().lock().unwrap().skip_404;
    // Retries: the engine's files; cloud links, and the failed files saved on disk when the queue
    // has none, are the app's
    match method {
        "retryBatch" if q.has_batch(&id) => {
            let n = q.retry_quiet(None, Some(&id), skip_404);
            if n > 0 {
                log_app("info", &format!("Retrying {n} tasks for batch '{id}'..."), "downloader", "");
            }
            return Some(retried(method, &id, n, Some(&id)));
        }
        // A file no longer in the queue: from the failed files saved for Retry Failed
        "retryTaskById" | "retrySingleTask" if !id.is_empty() && !q.has_key(&id) => {
            let picked: Vec<_> = super::journal::saved_failed()?
                .into_iter()
                .filter(|(t, _, _)| Queue::task_key(t) == id && !cloud_link(&t.url))
                .collect();
            if picked.is_empty() {
                return None;
            }
            let cells = q.adopt(picked);
            let keys = [id.clone()];
            let n = q.retry_quiet(Some(&keys), None, skip_404);
            if n > 0 {
                log_app("info", &format!("Retrying {n} selected task(s)..."), "downloader", "");
            }
            return Some(retried_restored(method, &id, n, &cells));
        }
        "retryTaskById" | "retrySingleTask" if q.has_key(&id) && !q.is_cloud_file(&id) => {
            let keys = [id.clone()];
            let n = q.retry_quiet(Some(&keys), None, skip_404);
            if n > 0 {
                log_app("info", &format!("Retrying {n} selected task(s)..."), "downloader", "");
            }
            return Some(retried(method, &id, n, None));
        }
        "retryFailed" => {
            let (mut n, mut any) = q.retry_failed_cells(skip_404, q.is_running(), 5, true, true);
            let mut cells = Vec::new();
            if !any {
                // Nothing failed in the queue: the failed files saved for Retry Failed come back to it
                // (cloud links are the app's)
                let saved: Vec<_> = super::journal::saved_failed()?
                    .into_iter()
                    .filter(|(t, _, _)| !cloud_link(&t.url) && !q.has_key(&Queue::task_key(t)))
                    .collect();
                if saved.is_empty() {
                    return None;
                }
                cells = q.adopt(saved);
                (n, any) = q.retry_failed_cells(skip_404, q.is_running(), 5, true, true);
                if !any {
                    return Some(retried_restored(method, &id, 0, &cells));
                }
            }
            log_app("info", &if n > 0 {
                format!("Retrying {n} failed task(s)...")
            } else {
                "Every failed file reached the retry limit (5); they stay in the Retry Failed list.".to_string()
            }, "downloader", "");
            return Some(if cells.is_empty() { retried(method, &id, n, None) } else { retried_restored(method, &id, n, &cells) });
        }
        // Clear failed (all, or the ones picked): gone from the engine's list; the app takes them
        // out of its own and of the failed files saved for Retry Failed
        "clearFailedTasks" => {
            let ids: Option<std::collections::HashSet<String>> = args.first().and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
            let removed = q.clear_failed(ids.as_ref());
            link().note("queue_action", json!([method, Value::Null, {"removed": removed, "ids": args.first().cloned().unwrap_or(Value::Null)}]));
            return Some(Value::Null);
        }
        _ => {}
    }
    let batch = matches!(method, "pauseBatch" | "resumeBatch" | "cancelBatch" | "removeBatch");
    let file = matches!(method, "pauseTask" | "resumeTask");
    if !(batch && q.has_batch(&id) || file && q.has_file(&id)) {
        return None;
    }
    let mut start = false;
    let n = match method {
        "pauseBatch" => {
            let n = q.pause_batch(&id);
            log_app("info", &format!("Paused batch '{id}' ({n} tasks paused)."), "downloader", "");
            n
        }
        "resumeBatch" => {
            let n = q.unpause_batch(&id);
            start = !q.is_running() && q.has_waiting(Some(&id));
            if n > 0 {
                log_app("info", &format!("Resumed batch '{id}' ({n} tasks restored)."), "downloader", "");
            }
            n
        }
        "cancelBatch" => q.cancel_batch(&id, true),
        "removeBatch" => q.remove_batch(&id),
        "pauseTask" => {
            let ok = q.pause_task(&id);
            if ok {
                log_app("info", &format!("Paused {}", q.file_name(&id)), "downloader", "");
            }
            ok as usize
        }
        _ => {
            let ok = q.unpause_task(&id);
            start = ok && !q.is_running() && q.has_waiting(None);
            if ok {
                log_app("info", &format!("Resumed {}", q.file_name(&id)), "downloader", "");
            }
            ok as usize
        }
    };
    if batch {
        let mut v = super::queue_view::global().lock().unwrap();
        if method == "pauseBatch" {
            v.paused_batches.insert(id.clone());
        } else {
            v.paused_batches.remove(&id);
        }
        v.dirty = true;
    }
    link().note("queue_action", json!([method, id, {"n": n, "start": start}]));
    Some(Value::Null)
}

/// A retry done: the window's paused post follows, the app is told (it retries the cloud links and
/// starts a run when files are waiting and nothing runs)
fn retried(method: &str, id: &str, n: usize, batch: Option<&str>) -> Value {
    let q = queue();
    if let Some(b) = batch {
        let mut v = super::queue_view::global().lock().unwrap();
        v.paused_batches.remove(b);
        v.dirty = true;
    }
    let start = !q.is_running() && q.has_waiting(batch);
    link().note("queue_action", json!([method, id, {"n": n, "start": start}]));
    Value::Null
}

/// The same for files put back from the saved list: the app takes them into its own (their keys
/// and how they are now)
fn retried_restored(method: &str, id: &str, n: usize, cells: &[Cell]) -> Value {
    let q = queue();
    let restored: Vec<Value> = cells
        .iter()
        .map(|c| {
            let key = c.lock().unwrap().task.key.clone();      // (released before task_dict locks it)
            json!([key, super::journal::task_dict(c)])
        })
        .collect();
    let start = !q.is_running() && q.has_waiting(None);
    link().note("queue_action", json!([method, id, {"n": n, "start": start, "restored": restored}]));
    Value::Null
}

/// A cloud host's link (MEGA, Drive, Dropbox, GoFile): the app downloads those
fn cloud_link(url: &str) -> bool {
    let u = url.to_lowercase();
    ["mega.nz", "mega.co.nz", "mega.io", "drive.google.com", "docs.google.com", "drive.usercontent.google.com", "dropbox.com", "gofile.io"]
        .iter()
        .any(|h| u.contains(h))
}

pub fn note_app(f: &str, args: Value) {
    link().note(f, args);
}

/// A line in the app's log (and log file), with details shown when it's expanded
pub fn log_app(level: &str, message: &str, category: &str, details: &str) {
    link().note("log", json!([level, message, true, category, details]));
}

/// Asks the app (for the window's requests it still handles); Err when it isn't connected or
/// the request failed there
pub fn ask_app(f: &str, args: Value) -> Result<Value, String> {
    let l = link();
    if l.out.lock().unwrap().is_none() {
        return Err("the engine isn't connected yet".into());
    }
    let v = l.ask(f, args);
    match v.get("$error").and_then(Value::as_str) {
        Some(e) => Err(e.to_string()),
        None => Ok(v),
    }
}

fn link() -> &'static Arc<Link> {
    static L: OnceLock<Arc<Link>> = OnceLock::new();
    L.get_or_init(Arc::default)
}

fn queue() -> &'static Arc<Queue> {
    static Q: OnceLock<Arc<Queue>> = OnceLock::new();
    Q.get_or_init(|| Queue::new(Arc::new(Remote(link().clone())), 4))
}

pub fn start() -> Result<ServiceInfo, String> {
    let fixed: u16 = std::env::var("PAWCHIVE_QUEUE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", fixed)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = std::env::var("PAWCHIVE_QUEUE_TOKEN").ok().filter(|t| t.len() >= 3).unwrap_or_else(crate::engine::new_token);
    let t = token.clone();
    std::thread::Builder::new()
        .name("queue-accept".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let t = t.clone();
                std::thread::spawn(move || serve(stream, &t));
            }
        })
        .map_err(|e| e.to_string())?;
    std::thread::Builder::new().name("queue-view".into()).spawn(follow_queue).map_err(|e| e.to_string())?;
    Ok(ServiceInfo { url: format!("tcp://127.0.0.1:{port}"), token })
}

fn serve(stream: TcpStream, token: &str) {
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let ok = serde_json::from_str::<Value>(&first).ok().and_then(|v| v.get("token").and_then(Value::as_str).map(|t| t == token)).unwrap_or(false);
    if !ok {
        return;
    }
    let l = link();
    // A new connection (the app restarted its engine) replaces the old one
    l.drop_all();
    *l.out.lock().unwrap() = Some(stream);
    l.send(json!({"t": "hello"}));
    // The queue's messages are handled in the order sent ("sync" then "start": a start overtaking
    // the sync of a big queue ran the old one), off the reading thread; calls each get a thread
    let (ordered, inbox) = channel::<Value>();
    std::thread::spawn(move || {
        for msg in inbox {
            handle(&msg);
        }
    });
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(msg) = serde_json::from_str::<Value>(buf.trim()) else { continue };
        if msg.get("t").and_then(Value::as_str) == Some("reply") {
            l.reply(msg["id"].as_u64().unwrap_or(u64::MAX), msg.get("value").cloned().unwrap_or(Value::Null));
            continue;
        }
        // Commands run off the reading thread, so answers to questions keep coming in
        // (calls on the queue's list itself go in order with the rest: a journal save or new files
        // after the sync before them; they never wait for the app)
        let on_list = matches!(msg.get("fn").and_then(Value::as_str), Some("queue_set" | "queue_append" | "journal_save" | "journal_clear" | "journal_unfinished" | "journal_summary" | "journal_load"));
        if matches!(msg.get("op").and_then(Value::as_str), Some("call" | "cancel_call")) && !on_list {
            std::thread::spawn(move || handle(&msg));
        } else if ordered.send(msg).is_err() {
            break;
        }
    }
    // The app went away: stop downloading for it
    queue().cancel();
    l.drop_all();
}

fn options_table(v: &Value) -> HashMap<String, Arc<Options>> {
    v.as_object().map(|m| m.iter().map(|(k, o)| (k.clone(), Arc::new(Options::from_dict(o)))).collect()).unwrap_or_default()
}

/// The app's files ({"tasks": [...], "options": {"o1": {...}}}) with their states and settings
fn incoming(msg: &Value) -> Vec<(Task, State, Option<Arc<Options>>)> {
    let table = options_table(&msg["options"]);
    let tasks = msg["tasks"].as_array().cloned().unwrap_or_default();
    tasks
        .into_iter()
        .filter_map(|t| {
            let task: Task = serde_json::from_value(t.clone()).ok()?;
            let mut st = State::default();
            let g = |k: &str| t.get(k).cloned().unwrap_or(Value::Null);
            if let Some(x) = g("status").as_str() {
                st.status = x.into();
            }
            st.error_msg = g("error_msg").as_str().unwrap_or("").into();
            st.retry_count = g("retry_count").as_u64().unwrap_or(0) as u32;
            st.downloaded_bytes = g("downloaded_bytes").as_u64().unwrap_or(0);
            st.http_status = g("http_status").as_u64().unwrap_or(0) as u16;
            let o = g("opt").as_str().and_then(|k| table.get(k).cloned());
            Some((task, st, o))
        })
        .collect()
}

/// New files for the queue (the app's "queue_append" call: {"tasks", "options"}): added here, those
/// already queued left out and save paths kept apart; the answer is what was added, as
/// {"added": [[key, save path], ...]}, for the app's list to follow. Nothing is started.
/// The app's list replaced (a new download, a restored session, files cleared…): files already here
/// keep their states, those being downloaded stay. Answers the list as it is now, by key: the app's
/// copy follows it.
fn queue_set(args: &Value) -> Result<Value, String> {
    let q = queue();
    q.sync(incoming(args), false);
    let keys: Vec<String> = q.cells().iter().map(|c| c.lock().unwrap().task.key.clone()).collect();
    Ok(json!({"keys": keys}))
}

fn queue_append(args: &Value) -> Result<Value, String> {
    let items = incoming(args).into_iter().map(|(t, st, o)| (t, Some(st), o)).collect();
    let added = queue().add(items, None);
    Ok(json!({"added": added}))
}

/// The window's view follows the engine's queue: marked when it may have changed, and read a
/// moment later (at most four times a second, and every two seconds anyway)
static VIEW_TOUCHED: AtomicBool = AtomicBool::new(true);

pub fn view_changed() {
    VIEW_TOUCHED.store(true, Ordering::Relaxed);
}

fn follow_queue() {
    let mut quiet = 0;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        quiet += 1;
        if !VIEW_TOUCHED.swap(false, Ordering::Relaxed) && quiet < 8 {
            continue;
        }
        quiet = 0;
        if super::queue_view::global().lock().unwrap().from_app {
            continue;
        }
        let rows: Vec<_> = queue().cells().iter().map(|c| super::queue_view::row_of(&c.lock().unwrap())).collect();
        super::queue_view::global().lock().unwrap().follow(rows);
    }
}

/// (marks the view as changed when dropped: after the message or request was handled)
struct Touch;

impl Drop for Touch {
    fn drop(&mut self) {
        view_changed();
    }
}

fn handle(msg: &Value) {
    let _touch = Touch;
    let q = queue();
    let s = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match msg.get("op").and_then(Value::as_str).unwrap_or("") {
        "sync" => q.sync(incoming(msg), msg["replace"].as_bool().unwrap_or(false)),
        "start" => {
            if let Some(n) = msg["workers"].as_u64() {
                q.set_max_workers(n as usize);
            }
            q.start(Arc::new(Options::from_dict(&msg["options"])));
        }
        "pause" => q.pause(),
        // (the cloud downloads' pause button)
        "cloud_pause" => super::cloud::PAUSED.store(msg["paused"].as_bool().unwrap_or(false), Ordering::Relaxed),
        "resume" => q.resume(),
        "cancel" => q.cancel(),
        "reset" => q.reset(),
        "workers" => q.set_max_workers(msg["n"].as_u64().unwrap_or(1) as usize),
        "state" => q.set_state(&s("key"), &s("status"), msg.get("error_msg").and_then(Value::as_str), msg.get("retry_count").and_then(Value::as_u64).map(|n| n as u32)),
        "batch" => q.set_batch_paused(&s("batch"), msg["paused"].as_bool().unwrap_or(false)),
        // A file changed on disk (a download finished, files moved): the Gallery's folder totals above it
        "gallery_stale" => crate::bridge::gallery::mark_stale(&s("path")),
        // settings.json's "windows_drive_mount": where Windows drives are on Linux
        "mount_pattern" => crate::path_translation::set_mount_pattern(&s("pattern")),
        "task_progress" => q.set_progress(
            &s("key"),
            msg["downloaded_bytes"].as_u64().unwrap_or(0),
            msg["file_size"].as_i64().unwrap_or(0),
            msg["progress_pct"].as_u64().unwrap_or(0).min(100) as u32,
            &s("speed_str"),
            &s("eta_str"),
        ),
        "view" => {
            let rows: Vec<super::queue_view::Row> = msg["rows"].as_array().into_iter().flatten().filter_map(|r| serde_json::from_value(r.clone()).ok()).collect();
            let order = msg["order"].as_array().map(|o| o.iter().filter_map(|k| k.as_str().map(str::to_string)).collect());
            let mut v = super::queue_view::global().lock().unwrap();
            v.apply(order, rows);
            if let Some(m) = msg.get("meta").filter(|m| m.is_object()) {
                v.paused_batches = m["paused"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(str::to_string)).collect();
                v.skip_404 = m["skip_404"].as_bool().unwrap_or(false);
                v.saved_failed = (m["saved_failed"][0].as_i64().unwrap_or(0), m["saved_failed"][1].as_i64().unwrap_or(0));
            }
        }
        "watchlist_view" => {
            let rows: Vec<crate::watchlist::view::Row> = msg["rows"].as_array().into_iter().flatten().filter_map(|r| serde_json::from_value(r.clone()).ok()).collect();
            let order = msg["order"].as_array().map(|o| o.iter().filter_map(|k| k.as_str().map(str::to_string)).collect());
            let mut v = crate::watchlist::view::global().lock().unwrap();
            v.apply(order, rows);
            if let Some(m) = msg.get("meta").filter(|m| m.is_object()) {
                v.base_dir = m["base_dir"].as_str().unwrap_or("").to_string();
                v.include_service = m["include_service"].as_bool().unwrap_or(true);
            }
            if msg["full"].as_bool().unwrap_or(false) {
                v.invalidate_disk();
            }
        }
        "cancel_call" => {
            let id = msg["id"].as_u64().unwrap_or(0);
            call_flags().lock().unwrap().entry(id).or_default().store(true, Ordering::Relaxed);
        }
        "call" => {
            let id = msg["id"].as_u64().unwrap_or(0);
            let args = &msg["args"];
            if s("fn").starts_with("journal_") {
                // (the settings folder, where the failed files for Retry Failed are saved too)
                super::journal::remember(args["path"].as_str().unwrap_or(""));
            }
            let out = match s("fn").as_str() {
                "plan" => plan_call(id, args),
                "queue_append" => queue_append(args),
                "queue_set" => queue_set(args),
                // The crash journal, from the engine's list: {"path", "settings", "batches", "status",
                // "generation"} → rows written (null: dropped, older than the last clear)
                "journal_save" => super::journal::save(
                    args["path"].as_str().unwrap_or(""),
                    &queue().cells(),
                    &args["settings"],
                    &args["batches"],
                    args["status"].as_str().unwrap_or("interrupted"),
                    args["generation"].as_u64().unwrap_or(0),
                )
                .map(|n| json!(n)),
                "journal_clear" => Ok(json!(super::journal::clear(args["path"].as_str().unwrap_or(""), args["generation"].as_u64().unwrap_or(0)))),
                // Reading it: {"path"} → whether a download was left unfinished / the recovery
                // prompt's summary / the saved queue (null: none)
                "journal_unfinished" => Ok(json!(super::journal::has_unfinished(args["path"].as_str().unwrap_or("")))),
                "journal_summary" => Ok(super::journal::summary(args["path"].as_str().unwrap_or("")).unwrap_or(Value::Null)),
                "journal_load" => Ok(super::journal::load(args["path"].as_str().unwrap_or("")).unwrap_or(Value::Null)),
                "site" => site_call(id, args),
                "watchlist_store" => crate::watchlist::store::call(args),
                "watchlist" => crate::watchlist::entries::call(args),
                "watchlist_check" => crate::watchlist::check::call(id, args),
                "watchlist_download" => crate::watchlist::check::download(id, args),
                "link_scan" => super::scan::call(id, args),
                "album_files" => super::albums::call(args),
                "cloud_download" => super::cloud::call(id, args),
                "vault" => crate::vault::call(id, args),
                "decompress" => crate::decompress::call(id, args),
                "ytdlp" => super::ytdlp::call(args),
                // The per-post groups as the window shows them (saved with the session and the export)
                "view_groups" => Ok(json!(super::queue_view::global().lock().unwrap().groups())),
                "selection_scan" => super::selection::scan(id, args),
                "selection_plan" => super::selection::plan(id, args),
                "watchlist_favorites" => crate::watchlist::favorites::call(id, args),
                other => Err(format!("unknown call {other}")),
            };
            call_flags().lock().unwrap().remove(&id);
            link().send(match out {
                Ok(v) => json!({"t": "result", "id": id, "value": v}),
                Err(e) => json!({"t": "result", "id": id, "error": e}),
            });
        }
        _ => {}
    }
}
