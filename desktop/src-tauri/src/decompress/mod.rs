//! The Decompressor: finding archives in folders, the disk space they need, and extracting them
//! with the bundled 7-Zip (password checks, progress, the extracted folder, the Recycle Bin for the
//! archive afterwards). A port of services/bulk_decompressor.py; the password bank is bank.rs.
//! The window's state (the list, selections, prompts) is still kept by the app.
//!
//! Checked against the Python Decompressor by tests/fixtures/decompress.json
//! (tools/golden_decompress.py), with real 7-Zip runs.

pub mod bank;

use crate::downloader::py::{abspath, basename, normcase, normpath, splitdrive, splitext};
use crate::downloader::service;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

pub const ARCHIVE_EXTENSIONS: &[&str] = &[".7z", ".zip", ".rar", ".tar", ".gz", ".bz2", ".xz", ".tgz", ".tbz2", ".txz", ".zst", ".iso", ".cab", ".lzma"];

/// A later part of a split archive (part 2, 3…): extracted with the first one
pub fn is_secondary_part(name: &str) -> bool {
    let m = re!(r"(?i)\.part(\d+)\.(?:rar|zip|7z|exe)$").captures(name).or_else(|| re!(r"\.(\d{3,})$").captures(name));
    m.and_then(|c| c[1].parse::<u64>().ok()).is_some_and(|n| n > 1)
}

/// The folder an archive is extracted into: its name without the archive's endings
pub fn clean_archive_stem(name: &str) -> String {
    let strip = |s: &str| s.trim_end_matches([' ', '.', '_']).to_string();
    let lower = name.to_lowercase();
    for c in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(c) {
            return strip(&name[..name.len() - c.len()]);
        }
    }
    if let Some(m) = re!(r"(?i)(?:\.part0*1\.(?:rar|zip|7z|exe)|(?:\.(?:7z|zip|rar|tar))?\.0*1)$").find(name) {
        let s = strip(&name[..m.start()]);
        return if s.is_empty() { "extracted_archive".into() } else { s };
    }
    let s = strip(splitext(name).0);
    if s.is_empty() { "extracted_archive".into() } else { s }
}

fn join(a: &str, b: &str) -> String {
    Path::new(a).join(b).to_string_lossy().into_owned()
}

fn exists(p: &str) -> bool {
    !p.is_empty() && Path::new(p).exists()
}

/// `base_name` in `parent`, numbered when it's taken ("name_1", "name_2"…)
fn unique_folder(parent: &str, base: &str) -> String {
    let t = join(parent, base);
    if !exists(&t) {
        return t;
    }
    (1..).map(|i| join(parent, &format!("{base}_{i}"))).find(|c| !exists(c)).unwrap()
}

// ── Finding archives ──────────────────────────────────────────────────────────────────────────

fn entries(dir: &str) -> Option<(Vec<String>, Vec<String>)> {
    let rd = std::fs::read_dir(dir).ok()?;
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if e.path().is_dir() { dirs.push(name) } else { files.push(name) }
    }
    Some((dirs, files))
}

/// The archives under each (folder, creator) location; deeper locations claim theirs first, and a
/// location inside another isn't searched again from the outer one. Later parts of split archives
/// are left out; an archive whose folder was extracted already starts unselected and done.
pub fn scan(locations: &[(String, String)], cancel: &AtomicBool) -> Vec<Value> {
    let valid: Vec<&(String, String)> = locations.iter().filter(|(f, _)| exists(f)).collect();
    let mut sorted = valid.clone();
    sorted.sort_by_key(|(f, _)| std::cmp::Reverse(normpath(f).split('\\').count()));
    let roots: HashSet<String> = valid.iter().map(|(f, _)| normcase(&normpath(f))).collect();
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut n = 0;
    for (folder, creator) in sorted {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let own = normcase(&normpath(folder));
        let others: HashSet<&String> = roots.iter().filter(|r| **r != own).collect();
        let mut stack = vec![folder.clone()];
        while let Some(root) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let Some((dirs, files)) = entries(&root) else { continue };
            let nroot = normcase(&normpath(&root));
            for f in files {
                let ext = splitext(&f).1.to_lowercase();
                if !(ARCHIVE_EXTENSIONS.contains(&ext.as_str()) || f.to_lowercase().ends_with(".001")) || is_secondary_part(&f) {
                    continue;
                }
                let full = normpath(&join(&root, &f));
                if !seen.insert(normcase(&full)) {
                    continue;
                }
                let size = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
                let target = join(&root, &clean_archive_stem(&f));
                let present = Path::new(&target).is_dir() && std::fs::read_dir(&target).is_ok_and(|mut d| d.next().is_some());
                n += 1;
                items.push(json!({
                    "id": format!("arc_{n}"), "path": full, "filename": f, "directory": root, "scanRoot": normpath(folder),
                    "size": size, "creator": if creator.is_empty() { basename(folder).to_string() } else { creator.clone() },
                    "selected": !present, "status": if present { "done" } else { "pending" },
                    "progress": if present { 100.0 } else { 0.0 }, "errorMessage": "", "targetDir": "", "password": "",
                    "extractedPresent": present, "extractedDir": if present { target } else { String::new() },
                }));
            }
            // (os.walk goes into the folders in order: pushed backwards)
            let keep: Vec<String> = dirs.into_iter().filter(|d| !others.contains(&normcase(&normpath(&join(&nroot, d))))).collect();
            for d in keep.into_iter().rev() {
                stack.push(join(&root, &d));
            }
        }
    }
    items
}

#[cfg(windows)]
fn free_bytes(dir: &str) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w: Vec<u16> = dir.encode_utf16().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
fn free_bytes(_dir: &str) -> Option<u64> {
    None
}

/// The selected archives by drive: their size and the space extracting needs (twice their size,
/// deleting them after doesn't lower it: they go to the Recycle Bin on the same drive)
pub fn disk_check(items: &[Value]) -> Vec<Value> {
    let mut drives: Vec<(String, Vec<&Value>)> = Vec::new();
    for it in items.iter().filter(|i| i["selected"].as_bool().unwrap_or(false)) {
        let full = abspath(it["path"].as_str().unwrap_or(""));
        let d = splitdrive(&full).0.to_string();
        let d = if d.is_empty() { full.chars().take(3).collect() } else { d }.to_uppercase();
        match drives.iter_mut().find(|(k, _)| *k == d) {
            Some((_, v)) => v.push(it),
            None => drives.push((d, vec![it])),
        }
    }
    drives
        .into_iter()
        .map(|(d, list)| {
            let total: u64 = list.iter().map(|i| i["size"].as_u64().unwrap_or(0)).sum();
            let need = total * 2;
            let root = if d.ends_with('\\') { d.clone() } else { format!("{d}\\") };
            let free = free_bytes(&root)
                .or_else(|| free_bytes(&crate::downloader::py::dirname(list[0]["path"].as_str().unwrap_or(""))))
                .unwrap_or(10 * 1024 * 1024 * 1024);
            json!({"drive": d, "archiveCount": list.len(), "totalArchiveSize": total, "estimatedRequired": need,
                   "freeBytes": free, "isSufficient": free >= need})
        })
        .collect()
}

// ── 7-Zip ─────────────────────────────────────────────────────────────────────────────────────

/// Whether 7-Zip's output says the archive is encrypted or the password is wrong
pub fn is_password_error(out: &str) -> bool {
    let l = out.to_lowercase();
    ["wrong password", "can not open encrypted archive", "data error in encrypted file", "enter password", "encrypted", "password"]
        .iter()
        .any(|p| l.contains(p))
}

fn command(seven: &str, args: &[String]) -> Command {
    let mut c = Command::new(seven);
    c.args(args).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // (no console window)
    }
    c
}

/// 7-Zip with its output and errors in one stream, as lines (\r, \n and \r\n end a line, as in
/// Python's text mode); `line` gets each one, and false stops (the process is killed)
fn run_lines(seven: &str, args: &[String], key: &str, line: &mut dyn FnMut(&str) -> bool) -> Result<Option<i32>, String> {
    let (mut reader, writer) = std::io::pipe().map_err(|e| e.to_string())?;
    let mut cmd = command(seven, args);
    cmd.stdout(writer.try_clone().map_err(|e| e.to_string())?).stderr(writer);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    drop(cmd);
    let child = Arc::new(Mutex::new(child));
    stopped_runs().lock().unwrap().remove(key);
    running().lock().unwrap().insert(key.to_string(), child.clone());
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut last_cr = false;
    let mut stopped = false;
    'read: loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' && last_cr {
                last_cr = false;
                continue;
            }
            last_cr = b == b'\r';
            if b == b'\n' || b == b'\r' {
                let mut l = String::from_utf8_lossy(&pending).into_owned();
                l.push('\n');
                pending.clear();
                if !line(&l) {
                    stopped = true;
                    break 'read;
                }
            } else {
                pending.push(b);
            }
        }
    }
    if !stopped && !pending.is_empty() {
        let l = String::from_utf8_lossy(&pending).into_owned();
        stopped = !line(&l);
    }
    if stopped {
        let _ = child.lock().unwrap().kill();
    }
    let code = child.lock().unwrap().wait().ok().and_then(|s| s.code());
    running().lock().unwrap().remove(key);
    Ok(code)
}

fn running() -> &'static Mutex<HashMap<String, Arc<Mutex<Child>>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<Mutex<Child>>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// The runs `cancel_all` stopped (their call may hear of the cancel only after 7-Zip has ended)
fn stopped_runs() -> &'static Mutex<HashSet<String>> {
    static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Whether `cancel_all` stopped the run `key` (asked once, after it ended)
fn was_stopped(key: &str) -> bool {
    stopped_runs().lock().unwrap().remove(key)
}

/// Stops every 7-Zip run now going
pub fn cancel_all() {
    for (key, c) in running().lock().unwrap().drain() {
        stopped_runs().lock().unwrap().insert(key);
        let _ = c.lock().unwrap().kill();
    }
}

/// Whether the archive needs a password, from its file list (fast: nothing is extracted)
pub fn probe(seven: &str, path: &str) -> (bool, String) {
    if seven.is_empty() || !exists(path) {
        return (false, String::new());
    }
    let mut out = String::new();
    let args: Vec<String> = ["l", "-slt", "-y", "-p-", path].iter().map(|s| s.to_string()).collect();
    let key = format!("probe:{path}");
    let Ok(code) = run_lines(seven, &args, &key, &mut |l| {
        out.push_str(l);
        true
    }) else {
        return (false, String::new());
    };
    if code != Some(0) && is_password_error(&out) {
        let t = out.trim();
        return (true, if t.is_empty() { "Encrypted".into() } else { t.lines().last().unwrap_or("").to_string() });
    }
    if re!(r"(?m)^Encrypted = \+").is_match(&out) {
        return (true, "Encrypted".into());
    }
    (false, String::new())
}

/// Whether `password` opens the archive (7-Zip's test, nothing written)
pub fn test_password(seven: &str, path: &str, password: &str, cancel: &AtomicBool) -> bool {
    if seven.is_empty() || !exists(path) {
        return false;
    }
    let mut cmd = command(seven, &["t".into(), "-y".into(), format!("-p{password}"), path.into()]);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else { return false };
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.code() == Some(0),
            Ok(None) => {}
            Err(_) => return false,
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Moves a file to the Recycle Bin
#[cfg(windows)]
pub fn to_recycle_bin(path: &str) -> bool {
    use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
    let mut from: Vec<u16> = abspath(path).encode_utf16().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    r == 0 && !op.fAnyOperationsAborted.as_bool() && !Path::new(path).exists()
}

#[cfg(not(windows))]
pub fn to_recycle_bin(_path: &str) -> bool {
    false
}

fn log(level: &str, msg: &str) {
    service::log_app(level, msg, "decompressor", "");
}

/// Extracts one archive into a folder named after it (the one it was extracted into before, when
/// there's one). `item` is the window's item; its targetDir, password and extracted folder are set.
/// (ok, error)
#[allow(clippy::too_many_arguments)]
pub fn extract(seven: &str, item: &mut Map<String, Value>, password: Option<&str>, threads: u32, delete_after: bool, overwrite: bool,
               probe_first: bool, progress: &mut dyn FnMut(f64), cancel: &AtomicBool, logf: &dyn Fn(&str, &str)) -> (bool, String) {
    let s = |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    if seven.is_empty() || !Path::new(seven).exists() {
        return (false, "7za.exe standalone binary not found in dependencies.".into());
    }
    let path = s(item, "path");
    if !exists(&path) {
        return (false, format!("Archive file not found: {path}"));
    }
    let pw = password.filter(|p| !p.is_empty()).map(str::to_string).unwrap_or_else(|| s(item, "password"));
    if pw.is_empty() && probe_first {
        let (enc, why) = probe(seven, &path);
        if enc {
            return (false, format!("ERROR: Can not open encrypted archive. Wrong password? ({why})"));
        }
    }
    let dir = s(item, "directory");
    let base = clean_archive_stem(&s(item, "filename"));
    let default_dir = join(&dir, &base);
    let earlier = s(item, "extractedDir");
    let (target, flag) = if overwrite && exists(&earlier) {
        (earlier, "-aoa")
    } else if overwrite && exists(&default_dir) {
        (default_dir, "-aos")
    } else {
        (unique_folder(&dir, &base), "-aos")
    };
    item.insert("targetDir".into(), json!(target));
    let ours = !Path::new(&target).exists();
    if let Err(e) = std::fs::create_dir_all(&target) {
        return (false, format!("Could not create destination directory: {}", py_os_error(&e)));
    }
    let cleanup = || {
        if ours && Path::new(&target).exists() {
            let _ = std::fs::remove_dir_all(&target);
        }
    };
    let mut args: Vec<String> = vec!["x".into(), "-y".into(), flag.into(), "-bsp1".into(), format!("-mmt={}", threads.max(1)), format!("-o{target}")];
    args.push(if pw.is_empty() { "-p-".into() } else { format!("-p{pw}") });
    args.push(path.clone());
    let mut lines: Vec<String> = Vec::new();
    let key = s(item, "id");
    let mut cancelled = false;
    let code = run_lines(seven, &args, &key, &mut |l| {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            return false;
        }
        lines.push(l.to_string());
        if lines.len() > 50 {
            lines.remove(0);
        }
        if let Some(c) = re!(r"(\d+)%").captures(l) {
            if let Ok(p) = c[1].parse::<f64>() {
                progress(p);
            }
        }
        true
    });
    let code = match code {
        Ok(c) => c,
        Err(e) => {
            cleanup();
            return (false, format!("Failed to launch 7za: {e}"));
        }
    };
    if was_stopped(&key) | cancelled || cancel.load(Ordering::Relaxed) {
        cleanup();
        return (false, "Cancelled by user".into());
    }
    let done = |item: &mut Map<String, Value>| {
        item.insert("extractedPresent".into(), json!(true));
        item.insert("extractedDir".into(), json!(target));
        if !pw.is_empty() {
            item.insert("password".into(), json!(pw));
        }
    };
    if code == Some(1) && !is_password_error(&lines.concat()) {
        done(item);
        progress(100.0);
        let tail: Vec<&str> = lines[lines.len().saturating_sub(6)..].iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        let tail: String = tail.join(" ").chars().take(300).collect();
        logf("warning", &format!("Extracted {} with warnings (the archive was kept): {tail}", s(item, "filename")));
        return (true, String::new());
    }
    if code == Some(0) {
        progress(100.0);
        done(item);
        if delete_after {
            if to_recycle_bin(&path) {
                logf("info", &format!("Moved the archive to the Recycle Bin after extracting it: {path}"));
            } else {
                logf("warning", &format!("Extracted, but couldn't move the archive to the Recycle Bin, so it was kept: {path}"));
            }
        }
        return (true, String::new());
    }
    cleanup();
    let tail = lines[lines.len().saturating_sub(10)..].concat();
    let tail = tail.trim();
    (false, format!("7-Zip exited with code {}: {}", code.unwrap_or(-1), if tail.is_empty() { "Decompression failed" } else { tail }))
}

/// "[WinError 5] Access is denied: 'path'" as Python words it (only the message is kept here)
fn py_os_error(e: &std::io::Error) -> String {
    e.to_string()
}

/// Call "decompress": {"op", …}
///   scan {"locations": [[folder, creator]]} → items
///   disk {"items"} → per drive
///   probe {"seven", "path"} → [encrypted, why]
///   test {"seven", "path", "password"} → bool (cancellable)
///   extract {"seven", "item", "password", "threads", "delete_after", "overwrite", "probe"} →
///     {"ok", "error", "item"}; progress notes {"progress": pct}
///   cancel_all
///   bank {...} (bank.rs)
pub fn call(id: u64, a: &Value) -> Result<Value, String> {
    let cancel = service::cancel_flag(id);
    let st = |k: &str| a[k].as_str().unwrap_or("").to_string();
    Ok(match a["op"].as_str().unwrap_or("") {
        "scan" => {
            let locs: Vec<(String, String)> = a["locations"].as_array().into_iter().flatten()
                .map(|l| (l[0].as_str().unwrap_or("").to_string(), l[1].as_str().unwrap_or("").to_string())).collect();
            json!(scan(&locs, &cancel))
        }
        "disk" => json!(disk_check(a["items"].as_array().map(Vec::as_slice).unwrap_or(&[]))),
        "probe" => {
            let (e, w) = probe(&st("seven"), &st("path"));
            json!([e, w])
        }
        "test" => json!(test_password(&st("seven"), &st("path"), &st("password"), &cancel)),
        "extract" => {
            let mut item = a["item"].as_object().cloned().unwrap_or_default();
            let pw = a["password"].as_str();
            let mut last = -1.0;
            let mut progress = |p: f64| {
                if p != last {
                    last = p;
                    service::progress_value(id, json!({"progress": p}));
                }
            };
            let (ok, err) = extract(&st("seven"), &mut item, pw, a["threads"].as_u64().unwrap_or(2) as u32,
                                    a["delete_after"].as_bool().unwrap_or(false), a["overwrite"].as_bool().unwrap_or(true),
                                    a["probe"].as_bool().unwrap_or(true), &mut progress, &cancel, &log);
            json!({"ok": ok, "error": err, "item": item})
        }
        "cancel_all" => {
            cancel_all();
            Value::Null
        }
        "bank" => return bank::call(a),
        other => return Err(format!("unknown decompress op {other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn b64(s: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }

    fn rel(v: &Value, base: &str) -> Value {
        match v {
            Value::String(s) => json!(s.replace(base, "<B>")),
            Value::Array(a) => Value::Array(a.iter().map(|x| rel(x, base)).collect()),
            Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), rel(x, base))).collect()),
            o => o.clone(),
        }
    }

    fn tmp(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("pawchive_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // (the long form of the folder, as Python's realpath gives it)
        normpath(&std::fs::canonicalize(&d).unwrap().to_string_lossy().replace(r"\\?\", ""))
    }

    fn files_of(base: &str) -> Map<String, Value> {
        let mut out = Map::new();
        fn walk(d: &Path, base: &Path, out: &mut Map<String, Value>) {
            let mut n = 0;
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                n += 1;
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    let r = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                    out.insert(r, json!(base64::engine::general_purpose::STANDARD.encode(std::fs::read(&p).unwrap())));
                }
            }
            if n == 0 && d != base {
                out.insert(format!("{}/", d.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/")), json!(""));
            }
        }
        walk(Path::new(base), Path::new(base), &mut out);
        out
    }

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/decompress.json")).unwrap()
    }

    #[test]
    fn names_scan_and_disk_like_python() {
        let fx = fixture();
        for n in fx["names"].as_array().unwrap() {
            let name = n[0].as_str().unwrap();
            assert_eq!(is_secondary_part(name), n[1].as_bool().unwrap(), "{name}");
            assert_eq!(clean_archive_stem(name), n[2].as_str().unwrap(), "{name}");
        }
        let base = tmp("scan");
        for t in fx["scan"]["tree"].as_array().unwrap() {
            let t = t.as_str().unwrap();
            let p = join(&base, &t.replace('/', "\\"));
            if t.ends_with('/') {
                std::fs::create_dir_all(&p).unwrap();
                continue;
            }
            std::fs::create_dir_all(crate::downloader::py::dirname(&p)).unwrap();
            std::fs::write(&p, vec![b'x'; t.len() * 100]).unwrap();
        }
        let locs: Vec<(String, String)> = fx["scan"]["locations"].as_array().unwrap().iter()
            .map(|l| (l[0].as_str().unwrap().replace("<B>", &base), l[1].as_str().unwrap().to_string())).collect();
        let items = scan(&locs, &AtomicBool::new(false));
        assert_eq!(rel(&json!(items), &base), fx["scan"]["items"]);
        let mut items = items;
        for i in items.iter_mut() {
            let dup = i["filename"] == "dup.zip";
            i["selected"] = json!(!dup);
        }
        let got: Vec<Value> = disk_check(&items).into_iter().map(|mut d| {
            let m = d.as_object_mut().unwrap();
            m.remove("freeBytes");
            m.remove("isSufficient");
            d
        }).collect();
        assert_eq!(json!(got), fx["disk"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn extracts_like_python() {
        let seven = concat!(env!("CARGO_MANIFEST_DIR"), "\\..\\..\\dependencies\\7z.exe");
        if !Path::new(seven).exists() {
            return; // (no 7-Zip here)
        }
        let seven = normpath(seven);
        let fx = fixture();
        let base = tmp("extract");
        for (name, data) in fx["extract"]["archives"].as_object().unwrap() {
            std::fs::write(join(&base, name), b64(data.as_str().unwrap())).unwrap();
        }
        let mut known: HashMap<String, Map<String, Value>> = HashMap::new();
        for r in fx["extract"]["runs"].as_array().unwrap() {
            let name = r["name"].as_str().unwrap();
            let path = join(&base, name);
            let pw = r["password"].as_str();
            let got = match r["op"].as_str().unwrap() {
                "test" => json!(test_password(&seven, &path, pw.unwrap(), &AtomicBool::new(false))),
                "probe" => {
                    let (e, w) = probe(&seven, &path);
                    json!([e, w])
                }
                _ => {
                    let item = known.entry(name.to_string()).or_insert_with(|| {
                        json!({"id": format!("arc_{name}"), "path": path, "filename": name, "directory": base, "scanRoot": "", "size": 0,
                               "creator": "", "selected": true, "status": "pending", "progress": 0.0, "errorMessage": "", "targetDir": "",
                               "password": "", "extractedPresent": false, "extractedDir": ""}).as_object().unwrap().clone()
                    });
                    let mut last: Option<f64> = None;
                    let threads = r["extra"]["threads"].as_u64().unwrap_or(2) as u32;
                    let probe_first = r["extra"]["probe"].as_bool().unwrap_or(true);
                    let (ok, err) = extract(&seven, item, pw, threads, false, true, probe_first, &mut |p| last = Some(p),
                                            &AtomicBool::new(false), &|_, _| {});
                    json!({"ok": ok, "error": err, "item": item, "last_progress": last})
                }
            };
            assert_eq!(rel(&got, &base), r["result"], "{} {name} {:?}", r["op"], pw);
            assert_eq!(Value::Object(files_of(&base)), r["files"], "files after {} {name}", r["op"]);
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
