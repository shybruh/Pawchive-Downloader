//! "appBridge.queueModel" and its "groupsModel": the window's download queue, from the Rust queue
//! view (downloader/queue_view.rs). The app keeps the view's files up to date. Pausing, resuming,
//! cancelling and removing a post or a file are done by the engine's queue (downloader/service.rs
//! queue_window), the app following; the rest (retries, clearing…) is passed to the app.

use super::native::{Events, NativeObject, Prop};
use crate::downloader::queue_view::{self, View, FILE_ROLES, GROUP_ROLES};
use crate::downloader::service::ask_app;
use serde_json::{json, Map, Value};
use std::sync::MutexGuard;
use std::time::Duration;

pub const QUEUE: &str = "appBridge.queueModel";
pub const GROUPS: &str = "appBridge.queueModel.groupsModel";

fn view() -> MutexGuard<'static, View> {
    queue_view::global().lock().unwrap()
}

fn page(rows: Vec<Value>, start: usize, count: Option<usize>) -> Value {
    let total = rows.len();
    let start = start.min(total);
    let end = count.map_or(total, |c| (start + c).min(total));
    json!({"total": total, "start": start, "rows": rows[start..end]})
}

/// The window's requests the app answers: (method, parameter names)
const METHODS: &[(&str, &[&str])] = &[
    ("removeBatch", &["batch_id"]),
    ("cancel_all_pending", &[]),
    ("cancelBatch", &["batch_id"]),
    ("pauseBatch", &["batch_id"]),
    ("resumeBatch", &["batch_id"]),
    ("pauseTask", &["file_id"]),
    ("resumeTask", &["file_id"]),
    ("isBatchPaused", &["batch_id"]),
    ("toggleBatchPause", &["batch_id"]),
    ("retryBatch", &["batch_id"]),
    ("clear", &[]),
    ("clearFailedTasks", &["selected_file_ids"]),
    ("retryFailed", &[]),
    ("retryTaskAt", &["visible_index"]),
    ("getFailedTasksList", &[]),
    ("prepareFailedTasksAsync", &["request_id"]),
    ("getFailedTasksPage", &["offset", "limit"]),
    ("retrySelected", &["selected_file_ids"]),
];

pub struct QueueModel;

impl NativeObject for QueueModel {
    fn props(&self) -> Vec<Prop> {
        let ro = |name| Prop { name, kind: "int", writable: false };
        let rw = |name, kind| Prop { name, kind, writable: true };
        vec![
            ro("totalCount"),
            ro("downloadingCount"),
            ro("completedCount"),
            ro("skippedCount"),
            ro("failedCount"),
            ro("pendingCount"),
            ro("count"),
            ro("groupsCount"),
            rw("filterStatus", "QString"),
            rw("minFileSize", "int"),
            rw("selectedBatchId", "QString"),
            rw("viewMode", "QString"),
            Prop { name: "groupsModel", kind: "QAbstractListModel*", writable: false },
        ]
    }
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        METHODS.iter().map(|(m, p)| (*m, p.to_vec())).collect()
    }
    fn signals(&self) -> Vec<&'static str> {
        // (sent by the app's model, passed through by the host)
        vec![
            "failedListReady", "retryRequested", "singleRetryRequested", "retrySelectedRequested", "batchRetryRequested",
            "batchCancelRequested", "batchRemoveRequested", "batchPauseRequested", "batchResumeRequested", "taskPauseRequested",
            "taskResumeRequested", "cleared",
        ]
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        match prop {
            "groupsModel" => Ok(json!({"$ref": GROUPS})),
            "groupsCount" => Ok(json!(view().groups().len())),
            p => view().props().get(p).cloned().ok_or_else(|| format!("unknown property {p:?}")),
        }
    }
    fn set(&self, prop: &str, value: Value) -> Result<Value, String> {
        let mut v = view();
        let text = || value.as_str().map(str::to_string).unwrap_or_else(|| if value.is_null() { String::new() } else { value.to_string() });
        match prop {
            "filterStatus" => v.filter_status = text(),
            "selectedBatchId" => v.selected_batch_id = text(),
            "viewMode" => v.view_mode = text(),
            "minFileSize" => v.min_file_size = value.as_i64().or_else(|| value.as_f64().map(|f| f as i64)).unwrap_or(0).max(0),
            p => return Err(format!("{p} is read-only")),
        }
        v.dirty = true;
        Ok(v.props()[prop].clone())
    }
    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        if !METHODS.iter().any(|(m, _)| *m == method) {
            return Err(format!("{QUEUE} has no method {method:?}"));
        }
        let (method, args) = match method {
            // The window's row number is the Rust list's: the app gets the file instead
            "retryTaskAt" => {
                let v = view();
                let i = args.first().and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
                let Some(&row) = v.visible().get(i) else { return Ok(Value::Null) };
                ("retryTaskById", vec![json!(v.rows()[row].file_id)])
            }
            "removeBatch" => {
                let mut v = view();
                if args.first().and_then(Value::as_str) == Some(v.selected_batch_id.as_str()) {
                    v.selected_batch_id.clear();
                    v.dirty = true;
                }
                (method, args)
            }
            _ => (method, args),
        };
        if let Some(v) = crate::downloader::service::queue_window(method, &args) {
            return Ok(v);
        }
        ask_app("queue_call", json!([method, args]))
    }
    fn model_roles(&self) -> Option<&'static [&'static str]> {
        Some(FILE_ROLES)
    }
    fn rows(&self, start: usize, count: Option<usize>) -> Result<Value, String> {
        let v = view();
        let vis = v.visible();
        let total = vis.len();
        let s = start.min(total);
        let e = count.map_or(total, |c| (s + c).min(total));
        let rows: Vec<Value> = vis[s..e].iter().map(|&i| v.file_row(i)).collect();
        Ok(json!({"total": total, "start": s, "rows": rows}))
    }
}

pub struct GroupsModel;

impl NativeObject for GroupsModel {
    fn model_roles(&self) -> Option<&'static [&'static str]> {
        Some(GROUP_ROLES)
    }
    fn rows(&self, start: usize, count: Option<usize>) -> Result<Value, String> {
        Ok(page(view().groups(), start, count))
    }
}

/// Tells the window what changed, a few times a second at most
pub fn start_pump(events: Events) {
    std::thread::Builder::new()
        .name("queue-view-events".into())
        .spawn(move || {
            let mut last = Map::new();
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let (props, files, groups) = {
                    let mut v = view();
                    if !v.dirty {
                        continue;
                    }
                    v.dirty = false;
                    let mut props = v.props();
                    let groups = v.groups().len();
                    props["groupsCount"] = json!(groups);
                    let files = props["count"].as_u64().unwrap_or(0) as usize;
                    (props, files, groups)
                };
                let changed: Map<String, Value> =
                    props.as_object().unwrap().iter().filter(|(k, val)| last.get(*k) != Some(val)).map(|(k, val)| (k.clone(), val.clone())).collect();
                if !changed.is_empty() {
                    events.props(QUEUE, Value::Object(changed));
                    last = props.as_object().unwrap().clone();
                }
                events.model(QUEUE, files);
                events.model(GROUPS, groups);
            }
        })
        .expect("queue view thread");
}
