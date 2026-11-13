//! The download's live numbers on "appBridge" (overallProgress, speedText, etaText,
//! savedBytesText, elapsedTimeText, filesCountText): worked out here from the engine's progress
//! reports (the bridge's _handle_progress), the app telling the rest (resetting them on Stop,
//! Pause, the end of a download; the old Python downloader's progress) with {"op": "live"}.

use crate::host::native::Events;
use serde_json::{json, Map, Value};
use std::sync::{Mutex, OnceLock};

pub const PROPS: &[(&str, &str)] = &[
    ("overallProgress", "double"), ("speedText", "QString"), ("etaText", "QString"),
    ("savedBytesText", "QString"), ("elapsedTimeText", "QString"), ("filesCountText", "QString"),
];

fn values() -> &'static Mutex<Map<String, Value>> {
    static V: OnceLock<Mutex<Map<String, Value>>> = OnceLock::new();
    V.get_or_init(|| {
        Mutex::new(Map::from_iter([
            ("overallProgress".to_string(), json!(0)), ("speedText".into(), json!("0 KB/s")), ("etaText".into(), json!("--")),
            ("savedBytesText".into(), json!("0 MB")), ("elapsedTimeText".into(), json!("0s")), ("filesCountText".into(), json!("")),
        ]))
    })
}

fn events() -> &'static OnceLock<Events> {
    static E: OnceLock<Events> = OnceLock::new();
    &E
}

/// The window's events go here (the host's)
pub fn set_events(e: Events) {
    let _ = events().set(e);
}

pub fn get(prop: &str) -> Option<Value> {
    values().lock().unwrap().get(prop).cloned()
}

/// New values (the ones of PROPS; others are left out); the window is told what changed
pub fn set(fields: &Map<String, Value>) {
    let mut changed = Map::new();
    {
        let mut v = values().lock().unwrap();
        for (k, val) in fields {
            if PROPS.iter().any(|(p, _)| p == k) && v.get(k) != Some(val) {
                v.insert(k.clone(), val.clone());
                changed.insert(k.clone(), val.clone());
            }
        }
    }
    if !changed.is_empty() {
        if let Some(e) = events().get() {
            e.props("appBridge", Value::Object(changed));
        }
    }
}

/// The numbers of one of the engine's progress reports (paused: no speed, no time left)
pub fn from_progress(info: &Value, paused: bool) -> Map<String, Value> {
    let get = |k: &str| info.get(k).filter(|v| !v.is_null());
    let text = |k: &str, d: &str| get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_else(|| d.to_string());
    let n = |k: &str| get(k).and_then(Value::as_i64).unwrap_or(0);
    let (completed, total) = (n("completed"), n("total"));
    let files = match get("files_count_text").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(f) => f.to_string(),
        None if total > 0 => format!("{completed}/{total}"),
        None => String::new(),
    };
    let mut m = Map::new();
    m.insert("overallProgress".into(), get("percent").or_else(|| get("progress")).cloned().unwrap_or(json!(0)));
    m.insert("savedBytesText".into(), json!(text("saved_str", "0 MB")));
    m.insert("speedText".into(), json!(if paused { "0 KB/s".into() } else { text("speed_str", "0 KB/s") }));
    m.insert("etaText".into(), json!(if paused { "--".into() } else { text("eta_str", "--") }));
    m.insert("filesCountText".into(), json!(files));
    m.insert("elapsedTimeText".into(), json!(text("elapsed_str", "0s")));
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_numbers() {
        let m = from_progress(&json!({"percent": 42.5, "saved_str": "12 MB", "speed_str": "3 MB/s", "eta_str": "1m 2s",
                                      "completed": 3, "total": 8, "elapsed_str": "9s"}), false);
        assert_eq!(Value::Object(m), json!({"overallProgress": 42.5, "savedBytesText": "12 MB", "speedText": "3 MB/s",
                                            "etaText": "1m 2s", "filesCountText": "3/8", "elapsedTimeText": "9s"}));
        let m = from_progress(&json!({"progress": 7, "speed_str": "3 MB/s", "files_count_text": "x of y"}), true);
        assert_eq!((m["overallProgress"].clone(), m["speedText"].clone(), m["etaText"].clone(), m["filesCountText"].clone()),
                   (json!(7), json!("0 KB/s"), json!("--"), json!("x of y")));
        assert_eq!(from_progress(&json!({}), false)["filesCountText"], json!(""));
    }
}
