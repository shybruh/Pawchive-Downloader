//! The window's view of the download queue: the file list (with its status / post / size filter),
//! the counts, and the per-post groups with their progress. A port of bridge/queue_model.py's
//! QueueModel and QueueGroupsModel, checked against them by tests/fixtures/queue_view.json
//! (python tools/golden_queue_view.py).
//!
//! The rows come from the app (`apply`): the files in queue order and every change to them.
//! The window reads it through the host's native objects (host/queue.rs).

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// One file of the queue, as the app sends it
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct Row {
    pub key: String,
    pub url: String,
    pub target_path: String,
    pub post_title: String,
    pub creator_name: String,
    pub service: String,
    pub post_id: String,
    pub file_id: String,
    pub batch_id: String,
    pub status: String,
    pub file_size: i64,
    pub downloaded_bytes: i64,
    pub error_msg: String,
    pub speed_str: String,
    pub eta_str: String,
    pub progress_pct: Value,
    pub retry_count: i64,
    pub http_status: i64,
}

impl Row {
    pub fn filename(&self) -> &str {
        super::py::basename(&self.target_path)
    }

    /// The batch the file list's post filter compares with
    fn list_batch(&self) -> String {
        if self.batch_id.is_empty() {
            format!("{}_{}_{}", self.service, self.creator_name, self.post_id)
        } else {
            self.batch_id.clone()
        }
    }

    /// The group the file is shown in
    pub fn group_id(&self) -> String {
        if !self.batch_id.is_empty() {
            return self.batch_id.clone();
        }
        let b = format!("{}_{}_{}", self.service, self.creator_name, self.post_id);
        let b = b.trim_matches('_');
        if b.is_empty() { "batch_default".into() } else { b.to_string() }
    }

    fn is_404(&self) -> bool {
        self.error_msg.to_lowercase().contains("404") || self.http_status == 404
    }
}

/// Python's QueueGroupsModel._format_size
pub fn format_size(b: i64) -> String {
    let g = 1024.0 * 1024.0 * 1024.0;
    let x = b as f64;
    if b <= 0 {
        "-".into()
    } else if x >= g {
        format!("{:.2} GB", x / g)
    } else if x >= 1024.0 * 1024.0 {
        format!("{:.1} MB", x / (1024.0 * 1024.0))
    } else if b >= 1024 {
        format!("{:.1} KB", x / 1024.0)
    } else {
        format!("{b} B")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counts {
    pub pending: usize,
    pub downloading: usize,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub failed_404: usize,
}

#[derive(Debug, Default)]
pub struct View {
    rows: Vec<Row>,
    at: HashMap<String, usize>,
    pub filter_status: String,
    pub min_file_size: i64,
    pub selected_batch_id: String,
    pub view_mode: String,
    pub paused_batches: HashSet<String>,
    /// "Don't retry files that weren't found (404)"
    pub skip_404: bool,
    /// The saved failed files (all, without 404s), counted when the queue has none
    pub saved_failed: (i64, i64),
    /// Something changed since the window was last told
    pub dirty: bool,
}

pub const FILE_ROLES: &[&str] = &[
    "filename", "postTitle", "creatorName", "service", "status", "progress", "fileSize", "downloadedBytes", "errorMsg", "url",
    "speed", "eta", "percentage", "originalIndex", "fileId", "retryCount", "batchId", "targetPath",
];

pub const GROUP_ROLES: &[&str] = &[
    "batchId", "creatorName", "postTitle", "service", "postId", "totalFiles", "completedFiles", "failedFiles", "downloadingFiles",
    "pendingFiles", "skippedFiles", "totalBytes", "downloadedBytes", "totalBytesStr", "downloadedBytesStr", "status", "totalProgress",
    "progress", "activeFileName", "activeFileProgressPct", "activeFileSpeed", "isPaused", "pausedFiles",
];

impl View {
    pub fn new() -> Self {
        Self { filter_status: "all".into(), view_mode: "grouped".into(), ..Default::default() }
    }

    /// The app's changes: `order` (when given) is the whole queue's keys in order; `rows` are
    /// new or changed files (a new file not in `order` goes at the end)
    pub fn apply(&mut self, order: Option<Vec<String>>, rows: Vec<Row>) {
        for r in rows {
            match self.at.get(&r.key) {
                Some(&i) => self.rows[i] = r,
                None => {
                    self.at.insert(r.key.clone(), self.rows.len());
                    self.rows.push(r);
                }
            }
        }
        if let Some(order) = order {
            let mut by_key: HashMap<String, Row> = self.rows.drain(..).map(|r| (r.key.clone(), r)).collect();
            self.rows = order.into_iter().filter_map(|k| by_key.remove(&k)).collect();
            self.at = self.rows.iter().enumerate().map(|(i, r)| (r.key.clone(), i)).collect();
        }
        self.dirty = true;
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for r in &self.rows {
            match r.status.as_str() {
                "downloading" | "retrying" => c.downloading += 1,
                "completed" => c.completed += 1,
                "skipped" => c.skipped += 1,
                "failed" | "cancelled" => {
                    c.failed += 1;
                    if r.is_404() {
                        c.failed_404 += 1;
                    }
                }
                "pending" | "paused" => c.pending += 1,
                _ => {}
            }
        }
        c
    }

    /// The "Retry failed" count
    pub fn failed_count(&self, c: &Counts) -> i64 {
        if c.failed > 0 {
            return if self.skip_404 { (c.failed - c.failed_404) as i64 } else { c.failed as i64 };
        }
        if self.skip_404 { self.saved_failed.1 } else { self.saved_failed.0 }
    }

    fn matches(&self, r: &Row) -> bool {
        if !self.selected_batch_id.is_empty() && r.list_batch() != self.selected_batch_id {
            return false;
        }
        if self.min_file_size > 0 && r.file_size.max(r.downloaded_bytes) < self.min_file_size {
            return false;
        }
        let s = r.status.as_str();
        match self.filter_status.as_str() {
            "downloading" => matches!(s, "downloading" | "retrying"),
            "completed" => s == "completed",
            "skipped" => s == "skipped",
            "failed" => matches!(s, "failed" | "cancelled"),
            "pending" => matches!(s, "pending" | "paused"),
            _ => true,
        }
    }

    /// The file list's rows (index into the queue)
    pub fn visible(&self) -> Vec<usize> {
        (0..self.rows.len()).filter(|&i| self.matches(&self.rows[i])).collect()
    }

    pub fn file_row(&self, i: usize) -> Value {
        let r = &self.rows[i];
        let progress = if r.file_size > 0 {
            (r.downloaded_bytes as f64 / r.file_size as f64).clamp(0.0, 1.0)
        } else if matches!(r.status.as_str(), "completed" | "skipped") {
            1.0
        } else {
            0.0
        };
        json!({
            "filename": r.filename(),
            "postTitle": r.post_title,
            "creatorName": r.creator_name,
            "service": r.service,
            "status": r.status,
            "progress": progress,
            "fileSize": format_size(r.file_size),
            "downloadedBytes": format_size(r.downloaded_bytes),
            "errorMsg": r.error_msg,
            "url": r.url,
            "speed": r.speed_str,
            "eta": r.eta_str,
            "percentage": r.progress_pct,
            "originalIndex": i,
            "fileId": r.file_id,
            "retryCount": r.retry_count,
            "batchId": r.list_batch(),
            "targetPath": r.target_path,
        })
    }

    /// The groups, in the order their first file joined the queue
    pub fn groups(&self) -> Vec<Value> {
        let mut order: Vec<String> = Vec::new();
        let mut members: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in self.rows.iter().enumerate() {
            let bid = r.group_id();
            members
                .entry(bid.clone())
                .or_insert_with(|| {
                    order.push(bid.clone());
                    Vec::new()
                })
                .push(i);
        }
        order.iter().map(|bid| self.group(bid, &members[bid])).collect()
    }

    fn group(&self, bid: &str, members: &[usize]) -> Value {
        let first = &self.rows[members[0]];
        let post_title = if bid.starts_with("artist_") || bid.starts_with("creator_") {
            "All Works / Posts".to_string()
        } else if first.post_title.is_empty() {
            "Media Collection".to_string()
        } else {
            first.post_title.clone()
        };
        let (mut completed, mut skipped, mut failed, mut downloading, mut pending, mut paused) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
        let (mut total_bytes, mut downloaded) = (0i64, 0i64);
        let mut active: Option<&Row> = None;
        for &i in members {
            let t = &self.rows[i];
            total_bytes += t.file_size.max(t.downloaded_bytes);
            downloaded += t.downloaded_bytes;
            match t.status.as_str() {
                "completed" => completed += 1,
                "skipped" => skipped += 1,
                "failed" => failed += 1,
                "downloading" | "retrying" => {
                    downloading += 1;
                    active.get_or_insert(t);
                }
                "cancelled" => {}
                "paused" => paused += 1,
                _ => pending += 1,
            }
        }
        let total = members.len();
        let is_paused = self.paused_batches.contains(bid) || (paused > 0 && downloading == 0);
        let status = if completed + skipped == total && total > 0 {
            if completed == 0 && skipped > 0 { "skipped" } else { "completed" }
        } else if is_paused {
            "paused"
        } else if downloading > 0 {
            "downloading"
        } else if failed > 0 && completed + skipped + failed == total {
            "failed"
        } else if completed > 0 {
            "partial"
        } else if skipped > 0 && skipped + pending == total {
            "skipped"
        } else if pending > 0 {
            "pending"
        } else {
            "cancelled"
        };
        let done = matches!(status, "completed" | "skipped");
        let total_progress = if total > 0 { (completed + skipped) as f64 / total as f64 } else if done { 1.0 } else { 0.0 };
        let progress = if total_bytes > 0 { downloaded as f64 / total_bytes as f64 } else if done { 1.0 } else { 0.0 };
        let mut g = Map::new();
        g.insert("batchId".into(), json!(bid));
        g.insert("creatorName".into(), json!(if first.creator_name.is_empty() { "Unknown Creator" } else { &first.creator_name }));
        g.insert("postTitle".into(), json!(post_title));
        g.insert("service".into(), json!(first.service));
        g.insert("postId".into(), json!(first.post_id));
        g.insert("totalFiles".into(), json!(total));
        g.insert("completedFiles".into(), json!(completed));
        g.insert("skippedFiles".into(), json!(skipped));
        g.insert("failedFiles".into(), json!(failed));
        g.insert("downloadingFiles".into(), json!(downloading));
        g.insert("pendingFiles".into(), json!(pending));
        g.insert("pausedFiles".into(), json!(paused));
        g.insert("isPaused".into(), json!(is_paused));
        g.insert("totalBytes".into(), json!(total_bytes));
        g.insert("downloadedBytes".into(), json!(downloaded));
        g.insert("totalBytesStr".into(), json!(format_size(total_bytes)));
        g.insert("downloadedBytesStr".into(), json!(format_size(downloaded)));
        g.insert("status".into(), json!(status));
        g.insert("totalProgress".into(), json!(total_progress));
        g.insert("progress".into(), json!(progress));
        match active {
            Some(t) => {
                g.insert("activeFileName".into(), json!(t.filename()));
                g.insert("activeFileProgressPct".into(), t.progress_pct.clone());
                g.insert("activeFileSpeed".into(), json!(if t.speed_str.is_empty() { "0 KB/s" } else { &t.speed_str }));
            }
            None => {
                g.insert("activeFileName".into(), json!(""));
                g.insert("activeFileProgressPct".into(), json!(0));
                g.insert("activeFileSpeed".into(), json!(""));
            }
        }
        Value::Object(g)
    }

    /// The model's properties as the window reads them
    pub fn props(&self) -> Value {
        let c = self.counts();
        json!({
            "totalCount": self.rows.len(),
            "downloadingCount": c.downloading,
            "completedCount": c.completed,
            "skippedCount": c.skipped,
            "failedCount": self.failed_count(&c),
            "pendingCount": c.pending,
            "count": self.visible().len(),
            "filterStatus": self.filter_status,
            "minFileSize": self.min_file_size,
            "selectedBatchId": self.selected_batch_id,
            "viewMode": self.view_mode,
        })
    }
}

/// The app's one queue view
pub fn global() -> &'static Mutex<View> {
    static V: OnceLock<Mutex<View>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(View::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-9,
            (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| close(a, b)),
            (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| close(v, w))),
            _ => a == b,
        }
    }

    /// python tools/golden_queue_view.py
    #[test]
    fn matches_python_queue_model() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/queue_view.json")).unwrap();
        let mut wrong = Vec::new();
        for c in &cases {
            let mut v = View::new();
            let rows: Vec<Row> = serde_json::from_value(c["rows"].clone()).unwrap();
            let keys = rows.iter().map(|r| r.key.clone()).collect();
            v.apply(Some(keys), rows);
            let st = &c["state"];
            v.filter_status = st["filter_status"].as_str().unwrap().into();
            v.min_file_size = st["min_file_size"].as_i64().unwrap();
            v.selected_batch_id = st["selected_batch_id"].as_str().unwrap().into();
            v.paused_batches = st["paused"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
            v.skip_404 = st["skip_404"].as_bool().unwrap();
            v.saved_failed = (st["saved_failed"][0].as_i64().unwrap(), st["saved_failed"][1].as_i64().unwrap());
            let mut props = v.props();
            for k in ["filterStatus", "minFileSize", "selectedBatchId", "viewMode"] {
                props.as_object_mut().unwrap().remove(k);
            }
            let files: Vec<Value> = v.visible().into_iter().map(|i| v.file_row(i)).collect();
            let got = json!({"props": props, "files": files, "groups": v.groups()});
            for k in ["props", "files", "groups"] {
                if !close(&got[k], &c["want"][k]) {
                    wrong.push(format!("{} / {k}\n  python: {}\n  rust:   {}", c["name"], c["want"][k], got[k]));
                }
            }
        }
        assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    #[test]
    fn apply_keeps_order_and_updates() {
        let r = |k: &str, s: &str| Row { key: k.into(), status: s.into(), ..Default::default() };
        let mut v = View::new();
        v.apply(Some(vec!["a".into(), "b".into()]), vec![r("a", "pending"), r("b", "pending")]);
        v.apply(None, vec![r("c", "pending"), r("a", "completed")]);
        assert_eq!(v.rows().iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(v.rows()[0].status, "completed");
        v.apply(Some(vec!["c".into(), "a".into()]), vec![]);
        assert_eq!(v.rows().iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["c", "a"]);
    }
}
