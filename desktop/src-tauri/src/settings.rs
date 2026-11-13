//! The app's settings file (settings.json): read with the defaults filled in, a leftover temporary
//! or test download folder dropped, paths saved on the other system translated, an unreadable file
//! kept aside; written crash-safely in the same form (the login cookie never written). The app's
//! settings as last loaded or saved are kept here for the Rust parts that read them.
//! A port of core/session_manager.py load_settings / save_settings, checked against them by
//! tools/golden_settings.py.

use crate::path_translation::Translator;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::sync::{Mutex, OnceLock};

/// Never written to settings.json (the login cookie is in the encrypted credentials vault)
const NEVER_SAVED: &[&str] = &["cookie"];

fn log(level: &str, msg: &str) {
    crate::downloader::service::log_app(level, msg, "session", "");
}

pub fn defaults(home: &str) -> Map<String, Value> {
    let dl = crate::downloader::py::join(&[home, "Downloads", "Pawchive"]);
    let v = json!({
        "download_dir": dl, "threads": 4, "cookie": "", "user_agent": "", "page_start": 1, "page_end": 999999,
        "filename_style": "post_title", "character_scope": "title", "skip_scope": "posts", "subfolder_per_post": true,
        "date_prefix": true, "file_index_prefix": false, "group_file_type": "none", "download_revisions": false,
        "compress_webp": false, "keep_duplicates": false, "scan_content_images": true, "download_dms": false,
        "fallback_to_thumbnails": false, "redownload_small_files": false, "dark_theme": true, "open_folder_on_complete": false,
        "play_completion_sound": false, "post_download_action": "none", "language": "auto", "storage_pool_enabled": false,
        "storage_pool_margin_gb": 10.0, "storage_pool_drives": [], "cookie_watchdog_enabled": true, "exact_extensions": "",
        "saved_custom_extensions": [], "gallery_bookmarks": [], "bandwidth_limit_enabled": false, "bandwidth_limit_value": 10.0,
        "bandwidth_limit_unit": "MB/s", "privacy_pin": "", "pending_pin_reset": false, "privacy_idle_minutes": 5
    });
    v.as_object().unwrap().clone()
}

/// A leftover download folder of a test or a temporary folder that's gone
fn bad_download_dir(d: &str) -> bool {
    let low = d.to_lowercase();
    d.contains("bridge_rebuild_test_") || d.contains("pawchive_tests_")
        || ((low.contains("temp") || low.contains("tmp")) && !std::path::Path::new(d).exists())
}

/// Folders saved on the other system in this system's form; the pattern for Windows drives on
/// Linux when the settings set one
fn localize(s: &mut Map<String, Value>, t: &mut Translator) -> Option<String> {
    let pattern = s.get("windows_drive_mount").and_then(Value::as_str).filter(|p| p.contains("{drive}")).map(str::to_string);
    if let Some(p) = &pattern {
        t.mount_pattern = p.clone();
    }
    if let Some(Value::String(d)) = s.get("download_dir") {
        let l = t.localize(d, None);
        s.insert("download_dir".into(), json!(l));
    }
    for key in ["storage_pool_drives", "gallery_bookmarks"] {
        if let Some(Value::Array(items)) = s.get(key).cloned() {
            let items: Vec<Value> = items.into_iter().map(|i| match i {
                Value::String(p) => json!(t.localize(&p, None)),
                Value::Object(mut o) => {
                    if let Some(Value::String(p)) = o.get("path").cloned() {
                        o.insert("path".into(), json!(t.localize(&p, None)));
                    }
                    Value::Object(o)
                }
                other => other,
            }).collect();
            s.insert(key.into(), Value::Array(items));
        }
    }
    pattern
}

/// The settings in `path` over the defaults: (settings, the Windows-drive pattern they set)
pub fn load(path: &str, home: &str, t: &mut Translator) -> (Map<String, Value>, Option<String>) {
    let mut s = defaults(home);
    let mut pattern = None;
    if !std::path::Path::new(path).exists() {
        return (s, pattern);
    }
    let read = std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()));
    match read {
        Ok(Value::Object(mut saved)) => {
            if let Some(Value::String(d)) = saved.get("download_dir") {
                if bad_download_dir(d) {
                    log("warning", &format!("Discarding invalid temporary/test download directory from settings: {d}"));
                    saved.remove("download_dir");
                }
            }
            for (k, v) in saved {
                s.insert(k, v);
            }
            pattern = localize(&mut s, t);
        }
        Ok(_) => {}
        Err(e) => {
            log("warning", &format!("Failed to load settings.json: {e}"));
            // (kept for inspection instead of being overwritten by the next save)
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let _ = std::fs::copy(path, format!("{path}.unreadable-{secs}"));
        }
    }
    (s, pattern)
}

// ── Writing it as Python's json.dumps(indent=2, ensure_ascii=False) does ───────────────────────

/// A float the way Python's repr writes it ("10.0", "2.5", "1e-07", "1e+16")
pub fn py_float(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{:e}", x.abs()); // shortest digits: "1.2345e15"
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1; // x = 0.digits × 10^decpt
    let sign = if x < 0.0 { "-" } else { "" };
    if decpt <= -4 || decpt > 16 {
        let (head, tail) = digits.split_at(1);
        let m = if tail.is_empty() { head.to_string() } else { format!("{head}.{tail}") };
        let e = decpt - 1;
        return format!("{sign}{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let n = digits.len() as i32;
    let body = if decpt <= 0 {
        format!("0.{}{digits}", "0".repeat((-decpt) as usize))
    } else if decpt >= n {
        format!("{digits}{}.0", "0".repeat((decpt - n) as usize))
    } else {
        format!("{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
    };
    format!("{sign}{body}")
}

fn write_value(v: &Value, indent: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => out.push_str(&i.to_string()),
            (_, Some(u)) => out.push_str(&u.to_string()),
            _ => out.push_str(&py_float(n.as_f64().unwrap_or(0.0))),
        },
        Value::String(s) => out.push_str(&serde_json::to_string(s).unwrap()),
        Value::Array(a) if a.is_empty() => out.push_str("[]"),
        Value::Object(o) if o.is_empty() => out.push_str("{}"),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                out.push_str(&pad(indent + 1));
                write_value(x, indent + 1, out);
            }
            out.push('\n');
            out.push_str(&pad(indent));
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                out.push_str(&pad(indent + 1));
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push_str(": ");
                write_value(x, indent + 1, out);
            }
            out.push('\n');
            out.push_str(&pad(indent));
            out.push('}');
        }
    }
}

/// The text settings.json gets for these settings
pub fn text_of(settings: &Map<String, Value>) -> String {
    let kept: Map<String, Value> = settings.iter().filter(|(k, _)| !NEVER_SAVED.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut out = String::new();
    write_value(&Value::Object(kept), 0, &mut out);
    out
}

/// The bytes settings.json gets: Python wrote it in text mode, so with Windows line endings on
/// Windows (the JSON's own strings never hold a raw line break)
pub fn file_text(settings: &Map<String, Value>) -> String {
    let t = text_of(settings);
    if cfg!(windows) { t.replace('\n', "\r\n") } else { t }
}

/// Writes a file crash-safely: a temporary file in the same folder, flushed, then swapped in
/// (retried a moment when Windows holds the target)
pub fn atomic_write(path: &str, text: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!("{name}.{}.tmp", uuid_ish()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        let _ = f.sync_all();
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    for attempt in 0..8 {
        match std::fs::rename(&tmp, p) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 7 && e.kind() == std::io::ErrorKind::PermissionDenied => {
                std::thread::sleep(std::time::Duration::from_millis(50 * (attempt + 1)));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e.to_string());
            }
        }
    }
    Err("couldn't replace the file".into())
}

fn uuid_ish() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}{:x}", t, N.fetch_add(1, Ordering::Relaxed))
}

// ── The app's settings ─────────────────────────────────────────────────────────────────────────

fn current() -> &'static Mutex<Option<Map<String, Value>>> {
    static C: OnceLock<Mutex<Option<Map<String, Value>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// One of the app's settings as last loaded or saved
#[allow(dead_code)] // (for the Rust parts that read settings, as they move)
pub fn get(key: &str) -> Option<Value> {
    current().lock().unwrap().as_ref().and_then(|m| m.get(key).cloned())
}

fn home() -> String {
    std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into())
}

/// Call "settings": {"op": "load", "path"} → {"settings", "mount_pattern"}; {"op": "save", "path",
/// "settings"} → null
pub fn call(a: &Value) -> Result<Value, String> {
    let path = a["path"].as_str().ok_or("no path")?;
    match a["op"].as_str().unwrap_or("") {
        "load" => {
            let mut t = Translator::here();
            let (s, pattern) = load(path, &home(), &mut t);
            if let Some(p) = &pattern {
                crate::path_translation::set_mount_pattern(p);
            }
            *current().lock().unwrap() = Some(s.clone());
            Ok(json!({"settings": s, "mount_pattern": pattern}))
        }
        "save" => {
            let s = a["settings"].as_object().cloned().ok_or("no settings")?;
            match atomic_write(path, &file_text(&s)) {
                Ok(()) => {
                    *current().lock().unwrap() = Some(s);
                    Ok(Value::Null)
                }
                Err(e) => {
                    log("error", &format!("Failed to save settings: {e}"));
                    Err(e)
                }
            }
        }
        other => Err(format!("unknown settings op {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_like_python() {
        for (x, want) in [(10.0, "10.0"), (2.5, "2.5"), (1e-7, "1e-07"), (0.0001, "0.0001"), (1e16, "1e+16"), (1234567890123456.0, "1234567890123456.0"),
                          (0.1, "0.1"), (-3.25, "-3.25"), (123.456, "123.456"), (1.5e300, "1.5e+300"), (5e-324, "5e-324"), (100.0, "100.0")] {
            assert_eq!(py_float(x), want, "{x}");
        }
    }

    #[test]
    fn matches_python_file() {
        let fx: Value = serde_json::from_str(include_str!("../tests/fixtures/settings.json")).unwrap();
        let d = std::env::temp_dir().join(format!("pawchive_settings_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tmpkeep")).unwrap();
        let base = crate::downloader::py::normpath(&std::fs::canonicalize(&d).unwrap().to_string_lossy().replace(r"\\?\", ""));
        let home = r"C:\Users\someone";
        let fix = |v: &Value| -> Value {
            serde_json::from_str(&v.to_string().replace("<B>", &base.replace('\\', "\\\\")).replace("<HOME>", &home.replace('\\', "\\\\"))).unwrap()
        };
        for l in fx["loads"].as_array().unwrap() {
            let name = l["name"].as_str().unwrap();
            let cfg = std::path::Path::new(&base).join(name.replace(' ', "_"));
            std::fs::create_dir_all(&cfg).unwrap();
            let path = cfg.join("settings.json").to_string_lossy().into_owned();
            if let Some(text) = l["file"].as_str() {
                std::fs::write(&path, text.replace("<B>", &base.replace('\\', "\\\\"))).unwrap();
            }
            let mut t = Translator::here();
            let (got, pattern) = load(&path, home, &mut t);
            assert_eq!(Value::Object(got), fix(&l["settings"]), "load {name}");
            assert_eq!(pattern.unwrap_or_else(|| "/mnt/{drive}".into()), l["mount_pattern"].as_str().unwrap(), "pattern {name}");
            let mut kept: Vec<String> = std::fs::read_dir(&cfg).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|f| f != "settings.json").map(|f| f.split('-').next().unwrap().to_string()).collect();
            kept.sort();
            assert_eq!(json!(kept), l["kept"], "kept {name}");
        }
        for (i, s) in fx["saves"].as_array().unwrap().iter().enumerate() {
            let path = std::path::Path::new(&base).join(format!("save{i}")).join("settings.json").to_string_lossy().into_owned();
            atomic_write(&path, &file_text(s["data"].as_object().unwrap())).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), s["text"].as_str().unwrap(), "save {i}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
