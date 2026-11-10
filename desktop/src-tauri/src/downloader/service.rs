//! The Rust download queue, run for the Python app until the bridge itself moves here.
//!
//! A local socket (like the transfer engine's). The app connects once with its token
//! (`{"token": "…"}`) and the line protocol runs both ways, one JSON object per line:
//!
//! From the app:
//!   `{"op": "sync", "tasks": [...], "options": {"o1": {...}}, "replace": true}`  the queue as it
//!   is now (with "replace", the app's statuses win: sent before a run starts)
//!   `{"op": "start", "options": {...}, "archive": true, "workers": 4}`
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
//!   refresh_link, special (yt-dlp / Telegram files), post_process, limit; while planning also
//!   known, archived, plan_cancelled
//!   `{"t": "note", "fn", "args"}`  the same without an answer: record_link, archive_password,
//!   add_passwords, log

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
    archive: AtomicBool,
    /// The download archive's file (from the app, with "start")
    archive_db: Mutex<String>,
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

/// The download archive at `path` (src/archive/), telling the app about its changes and errors
fn archive_at(l: &Arc<Link>, path: &str) -> Option<Arc<crate::archive::Archive>> {
    if path.is_empty() {
        return None;
    }
    let a = crate::archive::at(path);
    let errs = l.clone();
    a.set_error_sink(Arc::new(move |message: &str, error: &str| errs.note("archive_error", json!([message, error]))));
    Some(a)
}

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
    fn archive_enabled(&self) -> bool {
        self.0.archive.load(Ordering::Relaxed)
    }
    fn is_archived(&self, t: &Task) -> bool {
        let db = self.0.archive_db.lock().unwrap().clone();
        match archive_at(&self.0, &db) {
            Some(a) => a.is_archived(&t.service, &t.post_id, &t.file_id, &t.expected_sha256),
            None => self.0.ask("is_archived", json!([t.key])).as_bool().unwrap_or(false),
        }
    }
    fn record_file(&self, t: &Task, size: u64, path: &str) {
        let db = self.0.archive_db.lock().unwrap().clone();
        match archive_at(&self.0, &db) {
            Some(a) => {
                let ok = a.record_file(&crate::archive::Record {
                    service: t.service.clone(),
                    creator_id: t.user_id.clone(),
                    post_id: t.post_id.clone(),
                    file_id: t.file_id.clone(),
                    file_hash: t.expected_sha256.clone(),
                    filename: super::py::basename(&t.target_path).to_string(),
                    creator_name: t.creator_name.clone(),
                    post_title: t.post_title.clone(),
                    file_size: size as i64,
                    file_ext: String::new(),
                    file_path: path.to_string(),
                });
                if ok {
                    self.0.note("archive_changed", json!([]));
                }
            }
            None => {
                self.0.send(json!({"t": "record_file", "key": t.key, "size": size, "path": path}));
            }
        }
    }
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
    fn post_process(&self, t: &Task, _o: &Options) {
        self.0.ask("post_process", json!([t.key]));
    }
    fn changed(&self, c: &Cell) {
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
        let u = url.to_lowercase();
        ["mega.nz", "mega.co.nz", "mega.io", "drive.google.com", "docs.google.com", "drive.usercontent.google.com", "dropbox.com", "gofile.io"]
            .iter()
            .any(|h| u.contains(h))
    }
    fn log(&self, level: &str, message: &str) {
        self.0.send(json!({"t": "log", "level": level, "message": message}));
    }
}

/// What the planner looks up in the app while it plans one call's posts
struct PlanCtx {
    link: Arc<Link>,
    id: u64,
    archive: bool,
    /// The download archive (None: asked of the app)
    db: Option<Arc<crate::archive::Archive>>,
    sites: SiteClient,
    /// Known.txt and the learned franchise names ("": asked of the app)
    known_txt: String,
    franchise_cache: String,
    /// Stopping (a watchlist check's own flag), instead of asking the app
    stop: Option<Arc<AtomicBool>>,
}

impl Lookups for PlanCtx {
    fn known_hierarchy(&self, title: &str, tags: Option<&Value>, filenames: &[String], content: Option<&Value>) -> Option<(String, String)> {
        if !self.known_txt.is_empty() {
            let text = content.and_then(Value::as_str).unwrap_or("");
            if let Some(hit) = crate::known::with_list(&self.known_txt, &self.franchise_cache, |k| k.find_matching_hierarchy(title, tags, filenames, text)) {
                return hit;
            }
        }
        let a = self.link.ask("known", json!([title, tags, filenames, content]));
        let a = a.as_array()?;
        let s = |i: usize| a.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        Some((s(0), s(1)))
    }
    fn archive_enabled(&self) -> bool {
        self.archive
    }
    fn is_archived(&self, service: &str, post_id: &str, file_id: &str, sha256: &str) -> bool {
        match &self.db {
            Some(a) => a.is_archived(service, post_id, file_id, sha256),
            None => self.link.ask("archived", json!([service, post_id, file_id, sha256])).as_bool().unwrap_or(false),
        }
    }
    fn record_link(&self, service: &str, creator_id: &str, post_id: &str, url: &str, creator_name: &str, post_title: &str, link_title: &str) {
        match &self.db {
            Some(a) => {
                if a.record_link(service, creator_id, post_id, url, creator_name, post_title, link_title) {
                    self.link.note("archive_changed", json!([]));
                }
            }
            None => self.link.note("record_link", json!([service, creator_id, post_id, url, creator_name, post_title, link_title])),
        }
    }
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
        "unmatched_known": p.unmatched_known,
        "cancelled": p.cancelled,
    }))
}

/// The planner on these posts, with the plan call's arguments (options, request, archive, Known list)
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
    let archive = args["archive"].as_bool().unwrap_or(false);
    let db = if archive { archive_at(&l, args["archive_db"].as_str().unwrap_or("")) } else { None };
    let text = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let ctx = PlanCtx { link: l, id, archive, db, sites, known_txt: text("known_txt"), franchise_cache: text("franchise_cache"), stop };
    plan::build(posts, &o, &req, &ctx)
}

/// Cancel flags of the calls running now ({"op": "cancel_call", "id"} sets one)
fn call_flags() -> &'static Mutex<HashMap<u64, Arc<AtomicBool>>> {
    static F: OnceLock<Mutex<HashMap<u64, Arc<AtomicBool>>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

fn parsed_link(v: &Value) -> ParsedLink {
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
        std::thread::spawn(move || handle(&msg));
    }
    // The app went away: stop downloading for it
    queue().cancel();
    l.drop_all();
}

fn options_table(v: &Value) -> HashMap<String, Arc<Options>> {
    v.as_object().map(|m| m.iter().map(|(k, o)| (k.clone(), Arc::new(Options::from_dict(o)))).collect()).unwrap_or_default()
}

fn handle(msg: &Value) {
    let q = queue();
    let s = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match msg.get("op").and_then(Value::as_str).unwrap_or("") {
        "sync" => {
            let table = options_table(&msg["options"]);
            let tasks = msg["tasks"].as_array().cloned().unwrap_or_default();
            let incoming: Vec<(Task, State, Option<Arc<Options>>)> = tasks
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
                .collect();
            q.sync(incoming, msg["replace"].as_bool().unwrap_or(false));
        }
        "start" => {
            link().archive.store(msg["archive"].as_bool().unwrap_or(false), Ordering::Relaxed);
            *link().archive_db.lock().unwrap() = msg["archive_db"].as_str().unwrap_or("").to_string();
            if let Some(n) = msg["workers"].as_u64() {
                q.set_max_workers(n as usize);
            }
            q.start(Arc::new(Options::from_dict(&msg["options"])));
        }
        "pause" => q.pause(),
        "resume" => q.resume(),
        "cancel" => q.cancel(),
        "reset" => q.reset(),
        "workers" => q.set_max_workers(msg["n"].as_u64().unwrap_or(1) as usize),
        "state" => q.set_state(&s("key"), &s("status"), msg.get("error_msg").and_then(Value::as_str), msg.get("retry_count").and_then(Value::as_u64).map(|n| n as u32)),
        "batch" => q.set_batch_paused(&s("batch"), msg["paused"].as_bool().unwrap_or(false)),
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
            let out = match s("fn").as_str() {
                "plan" => plan_call(id, args),
                "site" => site_call(id, args),
                "watchlist_store" => crate::watchlist::store::call(args),
                "watchlist" => crate::watchlist::entries::call(args),
                "watchlist_check" => crate::watchlist::check::call(id, args),
                "archive" => {
                    archive_at(link(), args["path"].as_str().unwrap_or(""));
                    crate::archive::call(args)
                }
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
