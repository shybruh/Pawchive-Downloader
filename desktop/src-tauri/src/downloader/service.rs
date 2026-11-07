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
//!   `{"t": "reply", "id", "value"}`  the answer to a question below
//!
//! To the app:
//!   `{"t": "task", "key", ...state}`  a file changed (status, progress, where it's saved)
//!   `{"t": "progress", "info"}`, `{"t": "finished", "ok", "summary"}`, `{"t": "workers", "n"}`
//!   `{"t": "log", "level", "message"}`, `{"t": "checkpoint"}`, `{"t": "discard_recovery"}`
//!   `{"t": "record_file", "key", "size", "path"}`
//!   `{"t": "ask", "id", "fn", "args"}` for what the app still knows or does: is_archived, cookie,
//!   refresh_link, special (yt-dlp / Telegram files), post_process, limit

use super::fetch::{Cell, Ctl, Hooks, State};
use super::filters::Options;
use super::plan::Task;
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
    fn archive_enabled(&self) -> bool {
        self.0.archive.load(Ordering::Relaxed)
    }
    fn is_archived(&self, t: &Task) -> bool {
        self.0.ask("is_archived", json!([t.key])).as_bool().unwrap_or(false)
    }
    fn record_file(&self, t: &Task, size: u64, path: &str) {
        self.0.send(json!({"t": "record_file", "key": t.key, "size": size, "path": path}));
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
        _ => {}
    }
}
