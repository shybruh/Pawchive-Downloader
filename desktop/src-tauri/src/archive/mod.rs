//! The download archive (download_archive.db), being ported from core/archive_manager.py.
//!
//! First part: what downloading does with it, for every file: "was this downloaded before?" and
//! recording a downloaded file or a post's link. The database's tables, its summary tables and
//! search index (kept by SQLite triggers, see core/archive_index.py), the Archive tab's queries,
//! maintenance and repair are still Python's; both use the same file (SQLite's locking).
//! Checked against the Python archive by tools/golden_archive.py.

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

/// What went wrong, for the app (it shows the error and repairs a damaged database)
pub type ErrorSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct Archive {
    path: String,
    conn: Mutex<Conn>,
    /// Woken when the app opens the database again
    reopened: Condvar,
    on_error: Mutex<Option<ErrorSink>>,
}

#[derive(Default)]
struct Conn {
    c: Option<Connection>,
    /// The app closed the database (to repair, replace or clear it): wait until it's open again
    held: bool,
    /// The archive was turned off (or the app is closing): nothing is looked up or recorded
    off: bool,
}

/// One downloaded file, as it's recorded
#[derive(Debug, Default, Clone)]
pub struct Record {
    pub service: String,
    pub creator_id: String,
    pub post_id: String,
    pub file_id: String,
    pub file_hash: String,
    pub filename: String,
    pub creator_name: String,
    pub post_title: String,
    pub file_size: i64,
    pub file_ext: String,
    pub file_path: String,
}

/// datetime.now().isoformat()
fn now_iso() -> String {
    let t = chrono::Local::now();
    let micros = t.timestamp_subsec_micros();
    if micros == 0 {
        t.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        format!("{}.{micros:06}", t.format("%Y-%m-%dT%H:%M:%S"))
    }
}

impl Archive {
    fn new(path: &str) -> Self {
        Self { path: path.to_string(), conn: Mutex::default(), reopened: Condvar::new(), on_error: Mutex::new(None) }
    }

    pub fn set_error_sink(&self, sink: ErrorSink) {
        *self.on_error.lock().unwrap() = Some(sink);
    }

    fn failed(&self, what: &str, e: &str) {
        if let Some(sink) = self.on_error.lock().unwrap().clone() {
            sink(what, e);
        }
    }

    /// The connection (opened once; the app's archive tables must exist: the app creates them)
    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let mut g = self.conn.lock().unwrap();
        // (a repair can take minutes; downloads waited for it in the Python app too)
        let (g2, timeout) = self.reopened.wait_timeout_while(g, Duration::from_secs(600), |c| c.held && !c.off).unwrap();
        g = g2;
        if timeout.timed_out() {
            return Err("the download archive is being repaired or replaced".into());
        }
        if g.off {
            return Err("the download archive is off".into());
        }
        if g.c.is_none() {
            if !std::path::Path::new(&self.path).exists() {
                return Err("the download archive database doesn't exist yet".into());
            }
            let c = Connection::open(&self.path).map_err(|e| e.to_string())?;
            c.busy_timeout(Duration::from_secs(30)).map_err(|e| e.to_string())?;
            c.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
            c.pragma_update(None, "synchronous", "NORMAL").map_err(|e| e.to_string())?;
            g.c = Some(c);
        }
        let r = f(g.c.as_ref().unwrap()).map_err(|e| e.to_string());
        if r.is_err() {
            g.c = None; // (opened again next time: the file may have been repaired or replaced)
        }
        r
    }

    /// Whether the file was downloaded before: the same post and file, or the same content hash.
    /// An error counts as "no" (the file is downloaded again rather than skipped).
    fn is_off(&self) -> bool {
        self.conn.lock().unwrap().off
    }

    pub fn is_archived(&self, service: &str, post_id: &str, file_id: &str, file_hash: &str) -> bool {
        if self.is_off() {
            return false;
        }
        let r = self.with(|c| {
            let hit: Option<i64> = c
                .query_row("SELECT 1 FROM downloaded_files WHERE service = ? AND post_id = ? AND file_id = ? LIMIT 1;", params![service.to_lowercase(), post_id, file_id], |r| r.get(0))
                .optional()?;
            if hit.is_some() {
                return Ok(true);
            }
            let h = file_hash.trim();
            if h.is_empty() {
                return Ok(false);
            }
            Ok(c.query_row("SELECT 1 FROM downloaded_files WHERE file_hash = ? LIMIT 1;", [h], |r| r.get::<_, i64>(0)).optional()?.is_some())
        });
        r.unwrap_or_else(|e| {
            self.failed(&format!("Archive lookup failed (failing open): {e}"), &e);
            false
        })
    }

    /// Records a downloaded file (or updates its record: what's given replaces what was there)
    pub fn record_file(&self, r: &Record) -> bool {
        if self.is_off() {
            return false;
        }
        let now = now_iso();
        let hash = r.file_hash.trim().to_string();
        let filename = r.filename.trim().to_string();
        let path = r.file_path.trim().to_string();
        let (missing, verified): (i64, Option<String>) = if path.is_empty() { (-1, None) } else { (0, Some(now.clone())) };
        let ext = if r.file_ext.is_empty() && !filename.is_empty() {
            crate::downloader::py::splitext(&filename.to_lowercase()).1.to_string()
        } else {
            r.file_ext.to_lowercase()
        };
        let res = self.with(|c| {
            c.execute(
                "INSERT INTO downloaded_files
                 (service, creator_id, creator_name, post_id, post_title, file_id, file_hash, filename, file_size, file_ext, downloaded_at, file_path, is_missing, last_verified_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(service, post_id, file_id) DO UPDATE SET
                     creator_name = CASE WHEN excluded.creator_name != '' THEN excluded.creator_name ELSE downloaded_files.creator_name END,
                     post_title = CASE WHEN excluded.post_title != '' THEN excluded.post_title ELSE downloaded_files.post_title END,
                     file_hash = CASE WHEN excluded.file_hash != '' THEN excluded.file_hash ELSE downloaded_files.file_hash END,
                     filename = CASE WHEN excluded.filename != '' THEN excluded.filename ELSE downloaded_files.filename END,
                     file_size = CASE WHEN excluded.file_size > 0 THEN excluded.file_size ELSE downloaded_files.file_size END,
                     file_ext = CASE WHEN excluded.file_ext != '' THEN excluded.file_ext ELSE downloaded_files.file_ext END,
                     downloaded_at = excluded.downloaded_at,
                     file_path = CASE WHEN excluded.file_path != '' THEN excluded.file_path ELSE downloaded_files.file_path END,
                     is_missing = CASE WHEN excluded.file_path != '' THEN 0 ELSE downloaded_files.is_missing END,
                     last_verified_at = CASE WHEN excluded.file_path != '' THEN excluded.last_verified_at ELSE downloaded_files.last_verified_at END;",
                params![
                    r.service.to_lowercase(),
                    r.creator_id,
                    r.creator_name,
                    r.post_id,
                    r.post_title,
                    r.file_id,
                    hash,
                    filename,
                    r.file_size,
                    ext,
                    now,
                    path,
                    missing,
                    verified
                ],
            )
        });
        match res {
            Ok(_) => true,
            Err(e) => {
                self.failed(&format!("Failed to record file in download archive: {e}"), &e);
                false
            }
        }
    }

    /// Records an external link of a post (as a "file" whose id comes from the link)
    #[allow(clippy::too_many_arguments)]
    pub fn record_link(&self, service: &str, creator_id: &str, post_id: &str, url: &str, creator_name: &str, post_title: &str, link_title: &str) -> bool {
        let url = url.trim();
        if url.is_empty() {
            return false;
        }
        let hash: String = Sha256::digest(url.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let title = link_title.trim();
        self.record_file(&Record {
            service: service.into(),
            creator_id: creator_id.into(),
            post_id: post_id.into(),
            file_id: format!("link_{}", &hash[..16]),
            file_hash: hash.clone(),
            filename: if title.is_empty() { url.to_string() } else { title.to_string() },
            creator_name: creator_name.into(),
            post_title: post_title.into(),
            file_size: 0,
            file_ext: "link".into(),
            file_path: String::new(),
        })
    }

    /// Lets go of the database and waits until the app opens it again (it's repairing, replacing,
    /// clearing or closing it)
    pub fn hold(&self) {
        let mut g = self.conn.lock().unwrap();
        g.c = None;
        g.held = true;
    }

    /// The app opened the database again
    pub fn release(&self) {
        let mut g = self.conn.lock().unwrap();
        g.held = false;
        g.off = false;
        drop(g);
        self.reopened.notify_all();
    }

    /// The archive was turned off, or the app is closing: let go of the file, look nothing up
    pub fn turn_off(&self) {
        let mut g = self.conn.lock().unwrap();
        g.c = None;
        g.off = true;
        g.held = false;
        drop(g);
        self.reopened.notify_all();
    }

    /// Lets go of the database (opened again on the next use)
    #[cfg(test)]
    pub fn close(&self) {
        self.conn.lock().unwrap().c = None;
    }
}

/// The app's requests (the engine service's "archive" call): {"op", "path", ...}
pub fn call(a: &serde_json::Value) -> Result<serde_json::Value, String> {
    use serde_json::{json, Value};
    let path = a["path"].as_str().ok_or("no path")?;
    let arc = at(path);
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Ok(match a["op"].as_str().unwrap_or("") {
        "is_archived" => json!(arc.is_archived(&s("service"), &s("post_id"), &s("file_id"), &s("file_hash"))),
        "record_file" => json!(arc.record_file(&Record {
            service: s("service"),
            creator_id: s("creator_id"),
            post_id: s("post_id"),
            file_id: s("file_id"),
            file_hash: s("file_hash"),
            filename: s("filename"),
            creator_name: s("creator_name"),
            post_title: s("post_title"),
            file_size: a.get("file_size").and_then(Value::as_i64).unwrap_or(0),
            file_ext: s("file_ext"),
            file_path: s("file_path"),
        })),
        "record_link" => json!(arc.record_link(&s("service"), &s("creator_id"), &s("post_id"), &s("url"), &s("creator_name"), &s("post_title"), &s("link_title"))),
        "hold" => {
            arc.hold();
            Value::Null
        }
        "release" => {
            arc.release();
            Value::Null
        }
        "off" => {
            arc.turn_off();
            Value::Null
        }
        op => return Err(format!("unknown archive op {op:?}")),
    })
}

/// The archive at this path (the app has one; tests make their own)
pub fn at(path: &str) -> Arc<Archive> {
    static ALL: OnceLock<Mutex<HashMap<String, Arc<Archive>>>> = OnceLock::new();
    ALL.get_or_init(Default::default).lock().unwrap().entry(path.to_string()).or_insert_with(|| Arc::new(Archive::new(path))).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn s(v: &Value, k: &str) -> String {
        v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
    }

    /// python tools/golden_archive.py (the database it starts from is made by the Python archive)
    #[test]
    fn matches_python_archive() {
        let fx: Value = serde_json::from_str(include_str!("../../tests/fixtures/archive.json")).unwrap();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/target/archive_test");
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/download_archive.db");
        let empty = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/archive_empty.db");
        std::fs::copy(empty, &path).unwrap();
        let a = Archive::new(&path);
        let mut wrong = Vec::new();
        for (i, op) in fx["ops"].as_array().unwrap().iter().enumerate() {
            let x = &op["args"];
            let got = match op["fn"].as_str().unwrap() {
                "record_file" => json!(a.record_file(&Record {
                    service: s(x, "service"),
                    creator_id: s(x, "creator_id"),
                    post_id: s(x, "post_id"),
                    file_id: s(x, "file_id"),
                    file_hash: s(x, "file_hash"),
                    filename: s(x, "filename"),
                    creator_name: s(x, "creator_name"),
                    post_title: s(x, "post_title"),
                    file_size: x.get("file_size").and_then(Value::as_i64).unwrap_or(0),
                    file_ext: s(x, "file_ext"),
                    file_path: s(x, "file_path"),
                })),
                "record_link" => json!(a.record_link(&s(x, "service"), &s(x, "creator_id"), &s(x, "post_id"), &s(x, "url"), &s(x, "creator_name"), &s(x, "post_title"), &s(x, "link_title"))),
                "is_archived" => json!(a.is_archived(&s(x, "service"), &s(x, "post_id"), &s(x, "file_id"), &s(x, "file_hash"))),
                f => panic!("{f}"),
            };
            if got != op["result"] {
                wrong.push(format!("op {i} {}: python {} rust {}", op["fn"], op["result"], got));
            }
        }
        a.close();
        // The tables afterwards (times left out: they are "now")
        let c = Connection::open(&path).unwrap();
        for (t, cols) in fx["tables"].as_object().unwrap() {
            let cols: Vec<String> = cols["columns"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
            let mut q = c.prepare(&format!("SELECT {} FROM {t} ORDER BY {};", cols.join(", "), cols.join(", "))).unwrap();
            let rows: Vec<Value> = q
                .query_map([], |r| {
                    Ok(Value::Array(
                        (0..cols.len())
                            .map(|i| match r.get_ref(i).unwrap() {
                                rusqlite::types::ValueRef::Null => Value::Null,
                                rusqlite::types::ValueRef::Integer(x) => json!(x),
                                rusqlite::types::ValueRef::Real(x) => json!(x),
                                rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                                rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                            })
                            .collect(),
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            if json!(rows) != cols_rows(&fx["tables"][t]) {
                wrong.push(format!("table {t}\n  python: {}\n  rust:   {}", fx["tables"][t]["rows"], json!(rows)));
            }
        }
        // The search index found the same records
        for (q, want) in fx["searches"].as_object().unwrap() {
            // (as the Archive tab searches: LIKE over the index's columns, core/archive_index.py)
            let like = format!("%{q}%");
            let ids: Vec<i64> = c
                .prepare("SELECT rowid FROM archive_fts WHERE filename LIKE ? OR post_title LIKE ? OR creator_name LIKE ? OR creator_id LIKE ? OR post_id LIKE ? ORDER BY rowid;")
                .unwrap()
                .query_map(params![like, like, like, like, like], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            if json!(ids) != *want {
                wrong.push(format!("search {q:?}: python {want} rust {}", json!(ids)));
            }
        }
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
        assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    fn cols_rows(t: &Value) -> Value {
        t["rows"].clone()
    }
}
