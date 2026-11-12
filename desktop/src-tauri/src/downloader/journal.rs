//! The queue's crash journal written from the engine's own list: a port of the writing half of
//! core/queue_journal.py (QueueJournal.sync / clear) with the recovery summary's names from
//! core/recovery_manager.py (artist_name, _detect_platform), and its reading half (the "unfinished
//! download" prompt's check and summary, the queue Resume restores); the same SQLite file and format.
//!
//! Every file is a row keyed by its link and save path; a save writes only the rows whose file
//! changed (status, save path, error, retries, size, bytes downloaded) since the last save, removes
//! the files that left the queue, and on its first save the rows an earlier run left. When the
//! file was changed outside (emptied or deleted by the app), the next save writes everything.
//! A save older than the last clear (a "generation" from the app) is dropped, so a save that was on
//! its way when the download ended can't bring the journal back.
//!
//! Checked against the Python journal by tests/fixtures/journal.json (tools/golden_journal.py).

use super::fetch::Cell;
use super::py::{basename, dirname};
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

type Fp = (String, String, String, u32, i64, u64);

#[derive(Default)]
struct Journal {
    fp: HashMap<String, Fp>,
    disk_checked: bool,
    schema_ready: bool,
    generation: u64,
}

fn journals() -> &'static Mutex<HashMap<String, Journal>> {
    static J: OnceLock<Mutex<HashMap<String, Journal>>> = OnceLock::new();
    J.get_or_init(Default::default)
}

const GENERIC: &[&str] = &["bunkr", "erome", "nhentai", "unknown artist", "bunkr album", "erome album"];

/// RecoveryManager.artist_name: who a file is shown under (album sites: the album / folder)
pub fn artist_name(creator: &str, post_title: &str, target_path: &str) -> String {
    let c = if creator.is_empty() { "Unknown Artist" } else { creator };
    if GENERIC.contains(&c.to_lowercase().as_str()) {
        let t = post_title.to_lowercase();
        if !post_title.is_empty() && !["bunkr", "erome", "nhentai", "untitled", "bunkr album", "erome album"].contains(&t.as_str()) {
            return post_title.to_string();
        }
        if !target_path.is_empty() {
            let parent = basename(&dirname(target_path)).to_string();
            for prefix in ["Bunkr - ", "Erome - ", "nHentai - "] {
                if let Some(rest) = parent.strip_prefix(prefix) {
                    let rest = rest.trim();
                    if !rest.is_empty() {
                        return rest.to_string();
                    }
                    break;
                }
            }
        }
    }
    c.to_string()
}

/// RecoveryManager._detect_platform(service, url=url)
pub fn platform(service: &str, url: &str) -> String {
    let svc = service.to_lowercase();
    let u = url.to_lowercase();
    if svc == "bunkr" || u.contains("bunkr") || u.contains("balbums.st") {
        return "Bunkr".into();
    }
    if svc == "erome" || u.contains("erome") {
        return "Erome".into();
    }
    if svc == "nhentai" || u.contains("nhentai") {
        return "nHentai".into();
    }
    let base = if u.contains("pawchive") {
        "Pawchive"
    } else if u.contains("cum.st") || ["onlyfans", "fansly", "candfans"].contains(&svc.as_str()) {
        "cum.st"
    } else {
        "Pawchive"
    };
    let label = match svc.as_str() {
        "patreon" => "Patreon".to_string(),
        "fanbox" => "Pixiv Fanbox".into(),
        "fantia" => "Fantia".into(),
        "onlyfans" => "OnlyFans".into(),
        "fansly" => "Fansly".into(),
        "candfans" => "CandFans".into(),
        "subscribestar" => "SubscribeStar".into(),
        "boosty" => "Boosty".into(),
        "dlsite" => "DLsite".into(),
        "discord" => "Discord".into(),
        "afdian" => "Afdian".into(),
        "gumroad" => "Gumroad".into(),
        "" => "Unknown".into(),
        s => {
            let mut c = s.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    };
    format!("{label} ({base})")
}

/// DownloadTask.to_dict() of a queued file
fn task_dict(c: &Cell) -> Map<String, Value> {
    let it = c.lock().unwrap();
    let (t, st) = (&it.task, &it.st);
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        m.insert(k.into(), v);
    };
    put("url", json!(t.url));
    put("target_path", json!(t.target_path));
    put("filename", json!(basename(&t.target_path)));
    put("post_title", json!(t.post_title));
    put("creator_name", json!(t.creator_name));
    put("service", json!(t.service));
    put("post_id", json!(t.post_id));
    put("file_id", json!(t.file_id));
    put("file_size", json!(t.file_size));
    put("downloaded_bytes", json!(st.downloaded_bytes));
    put("status", json!(st.status));
    put("error_msg", json!(st.error_msg));
    put("retry_count", json!(st.retry_count));
    put("is_ytdlp", json!(t.is_ytdlp));
    put("is_telegram", json!(t.is_telegram));
    put("telegram_channel_id", if t.telegram_channel_id.is_null() { json!("") } else { t.telegram_channel_id.clone() });
    put("telegram_message_id", if t.telegram_message_id.is_null() { json!(0) } else { t.telegram_message_id.clone() });
    put("batch_id", json!(t.batch_id));
    put("post_url", json!(t.post_url));
    put("post_date", json!(t.post_date));
    put("user_id", json!(t.user_id));
    put("expected_sha256", json!(t.expected_sha256));
    put("fallback_urls", json!(t.fallback_urls));
    put("used_preview", json!(st.used_preview));
    put("options", it.options.as_ref().map(|o| Value::Object(o.as_map().clone())).unwrap_or(Value::Null));
    m
}

/// Python's "v not in (None, '', 0, False, [], {})"
fn empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Bool(b) => !b,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// One file's row: (key, status, artist, service, platform, total_bytes, downloaded, data, group_key)
pub fn row(c: &Cell) -> (String, String, String, String, String, i64, i64, String, String) {
    let d = task_dict(c);
    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let f_size = d["file_size"].as_i64().unwrap_or(0);
    let d_bytes = d["downloaded_bytes"].as_i64().unwrap_or(0);
    let compact: Map<String, Value> = d.iter().filter(|(k, v)| !empty(v) || *k == "status").map(|(k, v)| (k.clone(), v.clone())).collect();
    let artist = artist_name(&s("creator_name"), &s("post_title"), &s("target_path"));
    let service = s("service");
    let status = if s("status").is_empty() { "pending".to_string() } else { s("status") };
    (
        format!("{}\u{0}{}", s("url"), s("target_path")),
        status,
        artist.clone(),
        service.clone(),
        platform(&service, &s("url")),
        f_size.max(d_bytes),
        d_bytes,
        Value::Object(compact).to_string(),
        format!("{artist}\u{0}{service}").to_lowercase(),
    )
}

fn fingerprint(c: &Cell) -> (String, Fp) {
    let it = c.lock().unwrap();
    let key = format!("{}\u{0}{}", it.task.url, it.task.target_path);
    (key, (it.st.status.clone(), it.task.target_path.clone(), it.st.error_msg.clone(), it.st.retry_count, it.task.file_size, it.st.downloaded_bytes))
}

fn open(path: &str, j: &mut Journal) -> rusqlite::Result<Connection> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    conn.query_row("PRAGMA journal_mode=WAL;", [], |_| Ok(()))?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    if !j.schema_ready {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                key TEXT PRIMARY KEY, pos INTEGER NOT NULL, status TEXT NOT NULL, artist TEXT, service TEXT,
                platform TEXT, total_bytes INTEGER DEFAULT 0, downloaded INTEGER DEFAULT 0, data TEXT NOT NULL,
                group_key TEXT);",
        )?;
        let has_group: bool = conn.prepare("PRAGMA table_info(tasks);")?.query_map([], |r| r.get::<_, String>(1))?.flatten().any(|c| c == "group_key");
        if !has_group {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN group_key TEXT;")?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_journal_status ON tasks(status);
             CREATE INDEX IF NOT EXISTS idx_journal_group ON tasks(group_key);
             CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);",
        )?;
        j.schema_ready = true;
    }
    Ok(conn)
}

/// Saves the queue to the journal at `path` (QueueJournal.sync): the number of rows written, or
/// None when `generation` is older than the last clear (the save is dropped)
pub fn save(path: &str, cells: &[Cell], settings: &Value, batches: &Value, status: &str, generation: u64) -> Result<Option<usize>, String> {
    let mut all = journals().lock().unwrap();
    let j = all.entry(path.to_string()).or_default();
    if generation < j.generation {
        return Ok(None);
    }
    j.generation = generation;
    if !std::path::Path::new(path).exists() {
        j.fp.clear();
        j.schema_ready = false;
    }
    let mut conn = open(path, j).map_err(|e| e.to_string())?;
    // Rows changed outside (the app emptied the journal): everything is written again
    let on_disk: i64 = conn.query_row("SELECT COUNT(*) FROM tasks;", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if on_disk as usize != j.fp.len() {
        j.fp.clear();
    }
    let mut upserts = Vec::new();
    let mut now_keys: HashSet<String> = HashSet::with_capacity(cells.len());
    for (pos, c) in cells.iter().enumerate() {
        let (k, fp) = fingerprint(c);
        now_keys.insert(k.clone());
        if j.fp.get(&k) != Some(&fp) {
            j.fp.insert(k, fp);
            upserts.push((pos, c));
        }
    }
    let mut deletes: Vec<String> = j.fp.keys().filter(|k| !now_keys.contains(*k)).cloned().collect();
    for k in &deletes {
        j.fp.remove(k);
    }
    if !j.disk_checked {
        j.disk_checked = true;
        // (rows an earlier run left that aren't in this queue)
        let mut q = conn.prepare("SELECT key FROM tasks;").map_err(|e| e.to_string())?;
        let old: Vec<String> = q.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?.flatten().collect();
        drop(q);
        deletes.extend(old.into_iter().filter(|k| !now_keys.contains(k)));
    }
    let saved_at = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut del = tx.prepare("DELETE FROM tasks WHERE key = ?1;").map_err(|e| e.to_string())?;
        for k in &deletes {
            del.execute(params![k]).map_err(|e| e.to_string())?;
        }
        let mut up = tx
            .prepare(
                "INSERT INTO tasks (key, pos, status, artist, service, platform, total_bytes, downloaded, data, group_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(key) DO UPDATE SET pos = excluded.pos, status = excluded.status, artist = excluded.artist,
                     service = excluded.service, platform = excluded.platform, total_bytes = excluded.total_bytes,
                     downloaded = excluded.downloaded, data = excluded.data, group_key = excluded.group_key;",
            )
            .map_err(|e| e.to_string())?;
        for (pos, c) in &upserts {
            let r = row(c);
            up.execute(params![r.0, *pos as i64, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8]).map_err(|e| e.to_string())?;
        }
        let mut meta = tx.prepare("INSERT OR REPLACE INTO meta (k, v) VALUES (?1, ?2);").map_err(|e| e.to_string())?;
        for (k, v) in [("status", status.to_string()), ("saved_at", saved_at), ("settings", settings.to_string()), ("batches", batches.to_string())] {
            meta.execute(params![k, v]).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Some(upserts.len()))
}

/// Empties the journal at `path` (QueueJournal.clear: the file removed, else its rows): saves of an
/// older generation are dropped from now on. Whether there were rows.
pub fn clear(path: &str, generation: u64) -> bool {
    let mut all = journals().lock().unwrap();
    let j = all.entry(path.to_string()).or_default();
    j.generation = j.generation.max(generation);
    j.fp.clear();
    j.disk_checked = true;
    let p = std::path::Path::new(path);
    if !p.exists() {
        return false;
    }
    let had = Connection::open(path).and_then(|c| c.query_row("SELECT 1 FROM tasks LIMIT 1;", [], |_| Ok(()))).is_ok();
    let removed = [path.to_string(), format!("{path}-wal"), format!("{path}-shm")]
        .iter()
        .filter(|f| std::path::Path::new(f.as_str()).exists())
        .all(|f| std::fs::remove_file(f).is_ok());
    if removed {
        j.schema_ready = false;
    } else if let Ok(c) = Connection::open(path) {
        let _ = c.execute_batch("DELETE FROM tasks; DELETE FROM meta;");
    }
    had
}

// ── Reading it back (QueueJournal.has_unfinished / summary / load) ───────────────────────────

/// Files that never got their turn
const UNFINISHED: &str = "'pending', 'downloading', 'retrying'";

/// The journal opened for reading only (None: no journal, or one that can't be read)
fn reader(path: &str) -> Option<Connection> {
    if path.is_empty() || !std::path::Path::new(path).exists() {
        return None;
    }
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(Duration::from_secs(30)).ok()?;
    // (tables of an old file without them: nothing to read)
    conn.query_row("SELECT 1 FROM sqlite_master WHERE name = 'tasks';", [], |_| Ok(())).ok()?;
    Some(conn)
}

fn meta(conn: &Connection) -> HashMap<String, String> {
    let Ok(mut q) = conn.prepare("SELECT k, v FROM meta;") else { return HashMap::new() };
    q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// Whether the journal holds an unfinished download (the "resume?" prompt at start): files that
/// never got their turn (failed ones are kept for Retry Failed instead)
pub fn has_unfinished(path: &str) -> bool {
    let Some(conn) = reader(path) else { return false };
    if meta(&conn).get("status").map(String::as_str) == Some("completed") {
        return false;
    }
    conn.query_row(&format!("SELECT 1 FROM tasks WHERE status IN ({UNFINISHED}) LIMIT 1;"), [], |_| Ok(())).is_ok()
}

/// RecoveryManager._format_size: "0 B", "512.00 B", "1.50 KB"…
pub fn format_size(n: i64) -> String {
    if n <= 0 {
        return "0 B".into();
    }
    let mut x = n as f64;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if x.abs() < 1024.0 {
            return format!("{x:.2} {unit}");
        }
        x /= 1024.0;
    }
    format!("{x:.2} PB")
}

/// Python's round(x, 1)
fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

/// The recovery prompt's summary, counted by SQLite: totals and each creator's progress
pub fn summary(path: &str) -> Option<Value> {
    let conn = reader(path)?;
    let (n, done, failed, total_bytes, downloaded): (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(status = 'completed'), 0), COALESCE(SUM(status = 'failed'), 0),
                    COALESCE(SUM(total_bytes), 0), COALESCE(SUM(downloaded), 0) FROM tasks;",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .ok()?;
    if n == 0 {
        return None;
    }
    let mut q = conn
        .prepare(
            "SELECT MIN(artist), MIN(service), MIN(platform), COUNT(*), SUM(status = 'completed'), SUM(total_bytes), SUM(downloaded)
             FROM tasks GROUP BY group_key ORDER BY MIN(pos);",
        )
        .ok()?;
    let artists: Vec<Value> = q
        .query_map([], |r| {
            let text = |i: usize| r.get::<_, Option<String>>(i).map(Option::unwrap_or_default);
            let num = |i: usize| r.get::<_, Option<i64>>(i).map(Option::unwrap_or_default);
            let (name, svc, platform) = (text(0)?, text(1)?, text(2)?);
            let (a_total, a_done, a_bytes, a_down) = (num(3)?, num(4)?, num(5)?, num(6)?);
            Ok(json!({
                "name": if name.is_empty() { "Unknown Artist".to_string() } else { name },
                "service": svc, "platform": platform, "completed": a_done, "total": a_total,
                "percent": if a_total > 0 { round1(a_done as f64 / a_total as f64 * 100.0) } else { 0.0 },
                "formatted_downloaded": format_size(a_down), "formatted_total": format_size(a_bytes),
            }))
        })
        .ok()?
        .flatten()
        .collect();
    let saved_at = meta(&conn).get("saved_at").cloned().unwrap_or_default().replace('T', " ");
    Some(json!({
        "artists": artists, "total_files": n, "completed_files": done, "pending_files": n - done - failed,
        "failed_files": failed, "downloaded_bytes": downloaded, "total_bytes": total_bytes,
        "percent": round1(done as f64 / n as f64 * 100.0),
        "formatted_downloaded": format_size(downloaded), "formatted_total": format_size(total_bytes),
        "saved_at": saved_at,
    }))
}

/// Everything saved: {version, status, saved_at, settings, batches, tasks (in queue order)}, or
/// None when there's nothing
pub fn load(path: &str) -> Option<Value> {
    let conn = reader(path)?;
    let mut q = conn.prepare("SELECT data FROM tasks ORDER BY pos;").ok()?;
    let tasks: Vec<Value> = q
        .query_map([], |r| r.get::<_, String>(0))
        .ok()?
        .flatten()
        .filter_map(|d| serde_json::from_str(&d).ok())
        .collect();
    if tasks.is_empty() {
        return None;
    }
    let m = meta(&conn);
    let j = |k: &str, default: Value| m.get(k).filter(|v| !v.is_empty()).and_then(|v| serde_json::from_str(v).ok()).unwrap_or(default);
    Some(json!({
        "version": 2, "status": m.get("status").cloned().unwrap_or_else(|| "interrupted".into()),
        "saved_at": m.get("saved_at").cloned().unwrap_or_default(),
        "settings": j("settings", json!({})), "batches": j("batches", json!([])), "tasks": tasks,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::fetch::{self, State};
    use crate::downloader::filters::Options;
    use crate::downloader::plan::Task;
    use std::sync::Arc;

    /// The Python journal's rows and names for the same files (tools/golden_journal.py)
    #[test]
    fn matches_python_journal() {
        let fx: Value = serde_json::from_str(include_str!("../../tests/fixtures/journal.json")).unwrap();
        for p in fx["platforms"].as_array().unwrap() {
            assert_eq!(platform(p[0].as_str().unwrap(), p[1].as_str().unwrap()), p[2].as_str().unwrap(), "{p}");
        }
        for a in fx["artists"].as_array().unwrap() {
            assert_eq!(artist_name(a[0].as_str().unwrap(), a[1].as_str().unwrap(), a[2].as_str().unwrap()), a[3].as_str().unwrap(), "{a}");
        }
        let table: HashMap<String, Arc<Options>> =
            fx["options"].as_object().unwrap().iter().map(|(k, o)| (k.clone(), Arc::new(Options::from_dict(o)))).collect();
        let cells: Vec<Cell> = fx["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let task: Task = serde_json::from_value(t.clone()).unwrap();
                let c = fetch::cell(task);
                {
                    let mut it = c.lock().unwrap();
                    it.st = State {
                        status: t["status"].as_str().unwrap().into(),
                        error_msg: t["error_msg"].as_str().unwrap_or("").into(),
                        retry_count: t["retry_count"].as_u64().unwrap_or(0) as u32,
                        downloaded_bytes: t["downloaded_bytes"].as_u64().unwrap_or(0),
                        used_preview: t["used_preview"].as_bool().unwrap_or(false),
                        ..State::default()
                    };
                    it.options = t["opt"].as_str().and_then(|k| table.get(k).cloned());
                }
                c
            })
            .collect();
        let dir = std::env::temp_dir().join(format!("pawchive_journal_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("recovery_journal.db").to_string_lossy().to_string();
        assert_eq!(save(&path, &cells, &json!({"a": 1}), &json!([]), "in_progress", 0).unwrap(), Some(cells.len()));
        let conn = Connection::open(&path).unwrap();
        let mut q = conn.prepare("SELECT key, pos, status, artist, service, platform, total_bytes, downloaded, data, group_key FROM tasks ORDER BY pos;").unwrap();
        let got: Vec<Value> = q
            .query_map([], |r| {
                let data: String = r.get(8)?;
                Ok(json!([r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?,
                          r.get::<_, String>(5)?, r.get::<_, i64>(6)?, r.get::<_, i64>(7)?, serde_json::from_str::<Value>(&data).unwrap(), r.get::<_, String>(9)?]))
            })
            .unwrap()
            .flatten()
            .collect();
        let want = fx["rows"].as_array().unwrap();
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            assert_eq!(g, w);
        }
        // Read back as the Python journal reads it
        for sz in fx["sizes"].as_array().unwrap() {
            assert_eq!(format_size(sz[0].as_i64().unwrap()), sz[1].as_str().unwrap(), "{sz}");
        }
        let mut sum = summary(&path).unwrap();
        sum.as_object_mut().unwrap().remove("saved_at");
        assert_eq!(sum, fx["summary"]);
        assert_eq!(has_unfinished(&path), fx["has_unfinished"].as_bool().unwrap());
        let mut loaded = load(&path).unwrap();
        loaded.as_object_mut().unwrap().remove("saved_at");
        assert_eq!(loaded, fx["loaded"]);
        // Nothing changed: nothing written; one file changed: one row; a file gone: its row removed
        assert_eq!(save(&path, &cells, &json!({}), &json!([]), "in_progress", 0).unwrap(), Some(0));
        cells[0].lock().unwrap().st.status = "completed".into();
        assert_eq!(save(&path, &cells, &json!({}), &json!([]), "in_progress", 0).unwrap(), Some(1));
        assert_eq!(save(&path, &cells[1..], &json!({}), &json!([]), "in_progress", 0).unwrap(), Some(0));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks;", [], |r| r.get(0)).unwrap();
        assert_eq!(n as usize, cells.len() - 1);
        drop(q);
        drop(conn);
        // Emptied: a save from before the clear is dropped, a newer one writes everything
        assert!(clear(&path, 1));
        assert_eq!(save(&path, &cells, &json!({}), &json!([]), "in_progress", 0).unwrap(), None);
        assert!(!std::path::Path::new(&path).exists());
        assert_eq!(save(&path, &cells, &json!({}), &json!([]), "in_progress", 1).unwrap(), Some(cells.len()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
