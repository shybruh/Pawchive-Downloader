//! The watchlist's storage in SQLite (watchlist.db): a port of core/watchlist_store.py, checked
//! against it by tools/golden_watchlist_store.py (the same writes give the same database).
//!
//! Every creator is a row and a save writes only the creators that changed. Identical download
//! settings are stored once (keyed by the SHA-1 of their JSON). The updates a check found are
//! stored with the creator. A connection is opened per operation: an open file stays locked on
//! Windows (it couldn't be backed up, moved or deleted).

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
fn dumps(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

fn options_id(options_json: &str) -> String {
    Sha1::digest(options_json.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Object(o) => o.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::String(s) => s.is_empty(),
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
    }
}

#[derive(Default)]
struct State {
    schema_ready: bool,
    known_options: HashSet<String>,
}

pub struct Store {
    path: String,
    /// (one operation at a time, like the Python store's lock)
    state: Mutex<State>,
}

impl Store {
    pub fn new(path: &str) -> Self {
        Self { path: path.to_string(), state: Mutex::default() }
    }

    pub fn exists(&self) -> bool {
        Path::new(&self.path).exists()
    }

    fn open(&self, st: &mut State) -> Result<Connection, String> {
        if let Some(dir) = Path::new(&self.path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(&self.path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(30)).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(|e| e.to_string())?;
        if !st.schema_ready {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS creators (
                    rid INTEGER PRIMARY KEY,
                    pos INTEGER NOT NULL,
                    service TEXT,
                    user_id TEXT,
                    creator_name TEXT,
                    data TEXT NOT NULL,
                    options_id TEXT,
                    new_posts TEXT
                );
                CREATE TABLE IF NOT EXISTS options (id TEXT PRIMARY KEY, data TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);",
            )
            .map_err(|e| e.to_string())?;
            st.schema_ready = true;
        }
        Ok(conn)
    }

    pub fn has_rows(&self) -> Result<bool, String> {
        if !self.exists() {
            return Ok(false);
        }
        let mut st = self.state.lock().unwrap();
        let conn = self.open(&mut st)?;
        conn.query_row("SELECT 1 FROM creators LIMIT 1;", [], |_| Ok(())).optional().map(|r| r.is_some()).map_err(|e| e.to_string())
    }

    /// Every creator as [rid, pos, fields incl. "options", updates found], in watchlist order.
    /// Err when the database can't be read (the watchlist then falls back to its backup).
    pub fn load(&self) -> Result<Vec<Value>, String> {
        let mut st = self.state.lock().unwrap();
        let conn = self.open(&mut st)?;
        let check: Option<String> = conn.query_row("PRAGMA quick_check;", [], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        if check.as_deref() != Some("ok") {
            return Err(format!("watchlist.db is damaged ({})", check.unwrap_or_else(|| "no answer".into())));
        }
        let mut options: HashMap<String, Value> = HashMap::new();
        {
            let mut q = conn.prepare("SELECT id, data FROM options;").map_err(|e| e.to_string())?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
            for r in rows {
                let (id, data) = r.map_err(|e| e.to_string())?;
                options.insert(id, serde_json::from_str(&data).map_err(|e| e.to_string())?);
            }
        }
        st.known_options = options.keys().cloned().collect();
        let mut q = conn.prepare("SELECT rid, pos, data, options_id, new_posts FROM creators ORDER BY pos, rid;").map_err(|e| e.to_string())?;
        let rows = q
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            let (rid, pos, data, oid, new_posts) = r.map_err(|e| e.to_string())?;
            let mut d: Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
            let opts = oid.filter(|o| !o.is_empty()).and_then(|o| options.get(&o).cloned()).filter(Value::is_object).unwrap_or_else(|| json!({}));
            if let Some(m) = d.as_object_mut() {
                m.insert("options".into(), opts);
            }
            let posts: Value = match new_posts.filter(|s| !s.is_empty()) {
                Some(s) => serde_json::from_str(&s).map_err(|e| e.to_string())?,
                None => json!([]),
            };
            out.push(json!([rid, pos, d, posts]));
        }
        Ok(out)
    }

    pub fn get_meta(&self, key: &str, default: &str) -> Result<String, String> {
        let mut st = self.state.lock().unwrap();
        let conn = self.open(&mut st)?;
        let v: Option<Option<String>> = conn.query_row("SELECT v FROM meta WHERE k = ?;", [key], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        Ok(match v {
            Some(v) => v.unwrap_or_default(),
            None => default.to_string(),
        })
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        let mut st = self.state.lock().unwrap();
        let conn = self.open(&mut st)?;
        conn.execute("INSERT INTO meta (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v;", params![key, value])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Writes creators given as [rid, pos, fields without "options", options, updates found] and
    /// removes the rids in `deleted`, in one transaction; `replace_all` empties the table first
    pub fn write(&self, rows: &[Value], deleted: &[i64], replace_all: bool) -> Result<usize, String> {
        let mut new_options: Vec<(String, String)> = Vec::new();
        let mut prepared = Vec::new();
        let mut st = self.state.lock().unwrap();
        for r in rows {
            let (rid, pos, fields, opts, posts) = (&r[0], &r[1], &r[2], &r[3], &r[4]);
            let empty = Value::Object(Map::new());
            let o_json = dumps(if opts.is_null() { &empty } else { opts });
            let oid = (!is_empty(opts)).then(|| options_id(&o_json));
            if let Some(o) = &oid {
                if !st.known_options.contains(o) && !new_options.iter().any(|(k, _)| k == o) {
                    new_options.push((o.clone(), o_json.clone()));
                }
            }
            let text = |k: &str| fields.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            prepared.push((
                rid.as_i64().ok_or("a row without its id")?,
                pos.as_i64().unwrap_or(0),
                text("service"),
                text("user_id"),
                text("creator_name"),
                dumps(fields),
                oid,
                (!is_empty(posts)).then(|| dumps(posts)),
            ));
        }
        let mut conn = self.open(&mut st)?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        if replace_all {
            tx.execute("DELETE FROM creators;", []).map_err(|e| e.to_string())?;
        }
        for (id, data) in &new_options {
            tx.execute("INSERT OR IGNORE INTO options (id, data) VALUES (?, ?);", params![id, data]).map_err(|e| e.to_string())?;
        }
        {
            let mut q = tx
                .prepare(
                    "INSERT INTO creators (rid, pos, service, user_id, creator_name, data, options_id, new_posts)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(rid) DO UPDATE SET
                         pos = excluded.pos, service = excluded.service, user_id = excluded.user_id,
                         creator_name = excluded.creator_name, data = excluded.data,
                         options_id = excluded.options_id, new_posts = excluded.new_posts;",
                )
                .map_err(|e| e.to_string())?;
            for p in &prepared {
                q.execute(params![p.0, p.1, p.2, p.3, p.4, p.5, p.6, p.7]).map_err(|e| e.to_string())?;
            }
            let mut d = tx.prepare("DELETE FROM creators WHERE rid = ?;").map_err(|e| e.to_string())?;
            for rid in deleted {
                d.execute([rid]).map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        st.known_options.extend(new_options.into_iter().map(|(k, _)| k));
        Ok(prepared.len() + deleted.len())
    }

    pub fn drop_unused_options(&self) -> Result<(), String> {
        let mut st = self.state.lock().unwrap();
        let conn = self.open(&mut st)?;
        conn.execute("DELETE FROM options WHERE id NOT IN (SELECT DISTINCT options_id FROM creators WHERE options_id IS NOT NULL);", [])
            .map_err(|e| e.to_string())?;
        let mut q = conn.prepare("SELECT id FROM options;").map_err(|e| e.to_string())?;
        let ids = q.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        st.known_options = ids.filter_map(Result::ok).collect();
        Ok(())
    }

    /// A consistent copy of the database (SQLite's backup API, safe while it's in use)
    pub fn backup(&self, dest: Option<&str>) -> Result<String, String> {
        let dest = dest.map(str::to_string).unwrap_or_else(|| format!("{}.bak", self.path));
        let tmp = format!("{dest}.tmp");
        let r = (|| -> Result<(), String> {
            let mut st = self.state.lock().unwrap();
            let conn = self.open(&mut st)?;
            let mut out = Connection::open(&tmp).map_err(|e| e.to_string())?;
            {
                let b = rusqlite::backup::Backup::new(&conn, &mut out).map_err(|e| e.to_string())?;
                b.run_to_completion(i32::MAX, Duration::ZERO, None).map_err(|e| e.to_string())?;
            }
            drop(out);
            std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())
        })();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r.map(|_| dest)
    }

    /// Renames a damaged database (and its WAL files) out of the way; never deletes it
    pub fn set_aside(&self) -> Result<Option<String>, String> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let kept = format!("{}.damaged-{stamp}", self.path);
        for suffix in ["", "-wal", "-shm"] {
            let src = format!("{}{suffix}", self.path);
            if Path::new(&src).exists() {
                if let Err(e) = std::fs::rename(&src, format!("{kept}{suffix}")) {
                    let name = Path::new(&src).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    return Err(format!("Couldn't move the damaged {name} aside: {e}"));
                }
            }
        }
        let mut st = self.state.lock().unwrap();
        st.schema_ready = false;
        st.known_options.clear();
        Ok(Some(kept))
    }
}

/// The open stores, by path (the app has one; the tests make their own)
pub fn store(path: &str) -> Arc<Store> {
    static STORES: OnceLock<Mutex<HashMap<String, Arc<Store>>>> = OnceLock::new();
    STORES.get_or_init(Default::default).lock().unwrap().entry(path.to_string()).or_insert_with(|| Arc::new(Store::new(path))).clone()
}

/// The app's requests: {"op", "path", ...} (the engine service's "watchlist_store" call)
pub fn call(a: &Value) -> Result<Value, String> {
    let path = a["path"].as_str().ok_or("no path")?;
    let s = store(path);
    match a["op"].as_str().unwrap_or("") {
        "exists" => Ok(json!(s.exists())),
        "has_rows" => s.has_rows().map(|b| json!(b)),
        "load" => s.load().map(Value::Array),
        "get_meta" => s.get_meta(a["key"].as_str().unwrap_or(""), a["default"].as_str().unwrap_or("")).map(|v| json!(v)),
        "set_meta" => s.set_meta(a["key"].as_str().unwrap_or(""), a["value"].as_str().unwrap_or("")).map(|_| Value::Null),
        "write" => {
            let rows = a["rows"].as_array().cloned().unwrap_or_default();
            let deleted: Vec<i64> = a["deleted"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
            s.write(&rows, &deleted, a["replace_all"].as_bool().unwrap_or(false)).map(|n| json!(n))
        }
        "drop_unused_options" => s.drop_unused_options().map(|_| Value::Null),
        "backup" => s.backup(a["dest"].as_str()).map(|p| json!(p)),
        "set_aside" => s.set_aside().map(|p| json!(p)),
        op => Err(format!("unknown watchlist store op {op:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(path: &str) -> Value {
        if !Path::new(path).exists() {
            return json!({});
        }
        let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut out = Map::new();
        for t in ["creators", "options", "meta"] {
            let mut q = c.prepare(&format!("SELECT * FROM {t} ORDER BY 1;")).unwrap();
            let n = q.column_count();
            let rows: Vec<Value> = q
                .query_map([], |r| {
                    Ok(Value::Array(
                        (0..n)
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
            out.insert(t.into(), Value::Array(rows));
        }
        Value::Object(out)
    }

    /// python tools/golden_watchlist_store.py
    #[test]
    fn matches_python_watchlist_store() {
        let ops: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/watchlist_store.json")).unwrap();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/target/wlstore_test");
        let _ = std::fs::remove_dir_all(dir);
        let path = format!("{dir}/watchlist.db");
        let s = Store::new(&path);
        let mut wrong = Vec::new();
        for o in &ops {
            let a = &o["args"];
            let got = match o["op"].as_str().unwrap() {
                "has_rows" => json!(s.has_rows().unwrap()),
                "load" => Value::Array(s.load().unwrap()),
                "write" => {
                    let deleted: Vec<i64> = a["deleted"].as_array().unwrap().iter().map(|d| d.as_i64().unwrap()).collect();
                    json!(s.write(a["rows"].as_array().unwrap(), &deleted, a["replace_all"].as_bool().unwrap()).unwrap())
                }
                "set_meta" => {
                    s.set_meta(a["key"].as_str().unwrap(), a["value"].as_str().unwrap()).unwrap();
                    Value::Null
                }
                "get_meta" => json!(s.get_meta(a["key"].as_str().unwrap(), a["default"].as_str().unwrap()).unwrap()),
                "drop_unused_options" => {
                    s.drop_unused_options().unwrap();
                    Value::Null
                }
                op => panic!("{op}"),
            };
            if got != o["result"] {
                wrong.push(format!("{} answer\n  python: {}\n  rust:   {}", o["op"], o["result"], got));
            }
            let t = tables(&path);
            let (ts, ws) = (t.to_string(), o["tables"].to_string());
            if ts != ws {
                let at = ts.chars().zip(ws.chars()).take_while(|(a, b)| a == b).count();
                let around = |s: &str| s.chars().skip(at.saturating_sub(60)).take(160).collect::<String>();
                wrong.push(format!("{} tables, from character {at}\n  python: …{}\n  rust:   …{}", o["op"], around(&ws), around(&ts)));
            }
        }
        // The safety copy is a database with the same tables
        let bak = s.backup(None).unwrap();
        assert_eq!(tables(&bak).to_string(), tables(&path).to_string());
        let kept = s.set_aside().unwrap().unwrap();
        assert!(!Path::new(&path).exists() && Path::new(&kept).exists());
        let _ = std::fs::remove_dir_all(dir);
        assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
    }
}
