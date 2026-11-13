//! The download history (history.db): the History tab's sessions (a link's download: the creator,
//! the link, the site, how many files, when) and the downloaded files' ids. A port of
//! core/history_store.py and the session part of core/session_manager.py, checked against them by
//! tools/golden_history.py. The engine records a finished file itself (a few seconds' worth of
//! them written together).

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SESSIONS_SHOWN: usize = 500;

pub struct Store {
    pub path: String,
}

fn int_of(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        Value::Bool(b) => *b as i64,
        _ => 0,
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        o => o.to_string(),
    }
}

impl Store {
    pub fn new(path: &str) -> Self {
        Self { path: path.to_string() }
    }

    pub fn exists(&self) -> bool {
        std::path::Path::new(&self.path).exists()
    }

    fn open(&self) -> Result<Connection, String> {
        if let Some(d) = std::path::Path::new(&self.path).parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let c = Connection::open(&self.path).map_err(|e| e.to_string())?;
        c.busy_timeout(Duration::from_secs(30)).map_err(|e| e.to_string())?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS files (id TEXT PRIMARY KEY, at REAL) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS sessions (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 creator TEXT, url TEXT, service TEXT, files INTEGER, date TEXT
             );",
        )
        .map_err(|e| e.to_string())?;
        Ok(c)
    }

    pub fn add_files(&self, ids: &[String]) -> Result<(), String> {
        let ids: Vec<&String> = ids.iter().filter(|i| !i.is_empty()).collect();
        if ids.is_empty() {
            return Ok(());
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let mut c = self.open()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        for id in ids {
            tx.execute("INSERT OR IGNORE INTO files (id, at) VALUES (?, ?);", params![id, now]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn file_count(&self) -> i64 {
        if !self.exists() {
            return 0;
        }
        self.open().and_then(|c| c.query_row("SELECT COUNT(*) FROM files;", [], |r| r.get(0)).map_err(|e| e.to_string())).unwrap_or(0)
    }

    /// Sessions, oldest first: {"creator", "url", "service", "files", "date"}
    pub fn add_sessions(&self, entries: &[Value]) -> Result<(), String> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut c = self.open()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        for e in entries {
            let t = |k: &str| e.get(k).map(text).unwrap_or_default();
            tx.execute("INSERT INTO sessions (creator, url, service, files, date) VALUES (?, ?, ?, ?, ?);",
                       params![t("creator"), t("url"), t("service"), e.get("files").map(int_of).unwrap_or(0), t("date")])
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// The History tab's sessions, newest first
    pub fn recent_sessions(&self, limit: usize) -> Vec<Value> {
        if !self.exists() {
            return Vec::new();
        }
        let read = || -> Result<Vec<Value>, String> {
            let c = self.open()?;
            let mut q = c.prepare("SELECT creator, url, service, files, date FROM sessions ORDER BY seq DESC LIMIT ?;").map_err(|e| e.to_string())?;
            let rows = q.query_map(params![limit as i64], |r| {
                Ok(json!({"creator": r.get::<_, Option<String>>(0)?, "url": r.get::<_, Option<String>>(1)?,
                          "service": r.get::<_, Option<String>>(2)?, "files": r.get::<_, Option<i64>>(3)?,
                          "date": r.get::<_, Option<String>>(4)?}))
            }).map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        };
        read().unwrap_or_default()
    }

    /// An older version's history.json copied in; the number of rows
    pub fn import_legacy(&self, data: &Value) -> Result<usize, String> {
        let files: Vec<String> = data["downloaded_files"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(str::to_string)).collect();
        let mut sessions: Vec<Value> = data["download_history"].as_array().into_iter().flatten().filter(|s| s.is_object()).cloned().collect();
        self.add_files(&files)?;
        sessions.reverse(); // (the JSON list is newest first)
        self.add_sessions(&sessions)?;
        Ok(files.len() + sessions.len())
    }
}

// ── The app's history ──────────────────────────────────────────────────────────────────────────

struct Main {
    store: Option<Store>,
    pending: Vec<String>,
    flushing: bool,
}

fn main() -> &'static Mutex<Main> {
    static M: OnceLock<Mutex<Main>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Main { store: None, pending: Vec::new(), flushing: false }))
}

/// Where the app's history.db is (told by the app)
pub fn set_path(path: &str) {
    if !path.is_empty() {
        main().lock().unwrap().store = Some(Store::new(path));
    }
}

fn log(level: &str, msg: &str) {
    crate::downloader::service::log_app(level, msg, "session", "");
}

/// A link's download, for the History tab (written at once)
pub fn record_session(creator: &str, url: &str, service: &str, files: usize) -> bool {
    let m = main().lock().unwrap();
    let Some(st) = &m.store else { return false };
    let date = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    match st.add_sessions(&[json!({"creator": creator, "url": url, "service": service, "files": files, "date": date})]) {
        Ok(()) => true,
        Err(e) => {
            log("error", &format!("Failed to save download history: {e}"));
            false
        }
    }
}

/// A finished file: written with the others of the next few seconds
pub fn record_file(file_id: &str) {
    if file_id.is_empty() {
        return;
    }
    let mut m = main().lock().unwrap();
    if m.store.is_none() {
        return;
    }
    m.pending.push(file_id.to_string());
    if m.flushing {
        return;
    }
    m.flushing = true;
    drop(m);
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(3));
        flush();
    });
}

/// Writes the files recorded since the last write (also when the app closes)
pub fn flush() {
    let (files, path) = {
        let mut m = main().lock().unwrap();
        m.flushing = false;
        (std::mem::take(&mut m.pending), m.store.as_ref().map(|s| s.path.clone()))
    };
    let Some(path) = path else { return };
    if let Err(e) = Store::new(&path).add_files(&files) {
        log("error", &format!("Failed to save download history: {e}"));
        main().lock().unwrap().pending.splice(0..0, files);
    }
}

/// The History tab's sessions, newest first
pub fn recent() -> Vec<Value> {
    let path = main().lock().unwrap().store.as_ref().map(|s| s.path.clone());
    path.map(|p| Store::new(&p).recent_sessions(SESSIONS_SHOWN)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(path: &str) -> Value {
        let c = Connection::open(path).unwrap();
        let files: Vec<Value> = c.prepare("SELECT id FROM files ORDER BY id").unwrap()
            .query_map([], |r| Ok(json!([r.get::<_, String>(0)?]))).unwrap().map(Result::unwrap).collect();
        let sessions: Vec<Value> = c.prepare("SELECT seq, creator, url, service, files, date FROM sessions ORDER BY seq").unwrap()
            .query_map([], |r| Ok(json!([r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
                                         r.get::<_, i64>(4)?, r.get::<_, String>(5)?]))).unwrap().map(Result::unwrap).collect();
        json!({"files": files, "sessions": sessions})
    }

    #[test]
    fn matches_python_store() {
        let fx: Value = serde_json::from_str(include_str!("../tests/fixtures/history.json")).unwrap();
        let d = std::env::temp_dir().join(format!("pawchive_history_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let db = d.join("cfg").join("history.db").to_string_lossy().into_owned();
        let st = Store::new(&db);
        for (i, step) in fx["history"].as_array().unwrap().iter().enumerate() {
            let a = &step["args"];
            let got = match step["op"].as_str().unwrap() {
                "recent" => json!(st.recent_sessions(a["limit"].as_u64().unwrap() as usize)),
                "file_count" => json!(st.file_count()),
                "add_files" => {
                    let ids: Vec<String> = a["ids"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
                    st.add_files(&ids).unwrap();
                    Value::Null
                }
                "add_sessions" => {
                    st.add_sessions(a["entries"].as_array().unwrap()).unwrap();
                    Value::Null
                }
                "import_legacy" => json!(st.import_legacy(&a["data"]).unwrap()),
                op => panic!("{op}"),
            };
            assert_eq!(got, step["result"], "step {i} {}", step["op"]);
            if !step["rows"].is_null() {
                assert_eq!(rows(&db), step["rows"], "rows after step {i} {}", step["op"]);
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
