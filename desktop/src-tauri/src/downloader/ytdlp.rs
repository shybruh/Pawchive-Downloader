//! Embedded videos (YouTube, Vimeo, Streamable, RedGifs…) downloaded with yt-dlp: a port of
//! services/ytdlp_manager.py and the yt-dlp part of FileDownloader._download_single_file.
//!
//! The app says where yt-dlp is and whose browser sign-in to use (Hooks::ytdlp); yt-dlp is
//! installed from GitHub when it's missing and updated in the background at start-up (`call`
//! "update"). Each run sits in its own job (proc_group.rs), so stopping it ends the whole program:
//! the standalone yt-dlp.exe is a launcher that runs the real one as its child.

use super::fetch::{Cell, Ctl, Hooks};
use super::filters::{self as f, Options};
use super::py::{basename, dirname, float, join, splitext};
use super::service;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Browsers yt-dlp can read a sign-in from
pub const BROWSERS: &[&str] = &["firefox", "chrome", "edge", "brave", "opera", "vivaldi", "chromium", "safari"];
/// yt-dlp's own messages when it can't read a browser's sign-in (not a video's "sign in to watch")
const COOKIE_ERRORS: &[&str] = &["cookie database", "cookies database", "decrypt with dpapi", "failed to decrypt",
    "unsupported browser specified for cookies", "keyring"];
const NOT_A_VIDEO_ERRORS: &[&str] = &["unsupported url", "no video", "there's no video", "no media found", "not a video", "has no video"];
/// Embedded-video sites that no longer exist
const DEAD_VIDEO_HOSTS: &[&str] = &["gfycat.com"];
/// Sites that embed Vimeo players: domain-locked embeds only play for the creator's own site
const EMBED_REFERERS: &[(&str, &str)] = &[
    ("patreon", "https://www.patreon.com/"),
    ("fanbox", "https://www.fanbox.cc/"),
    ("fantia", "https://fantia.jp/"),
    ("subscribestar", "https://www.subscribestar.com/"),
    ("gumroad", "https://gumroad.com/"),
    ("boosty", "https://boosty.to/"),
];
const PROGRESS_TEMPLATE: &str =
    "download:PROGRESS:%(progress.downloaded_bytes)s/%(progress.total_bytes,progress.total_bytes_estimate)s:%(progress.speed)s:%(progress.eta)s";
const MIN_BINARY: u64 = 1024 * 1024;
#[cfg(windows)]
const BINARY: &str = "yt-dlp.exe";
#[cfg(not(windows))]
const BINARY: &str = "yt-dlp";

fn log(level: &str, msg: &str) {
    service::log_app(level, msg, "ytdlp", "");
}

/// yt-dlp couldn't read the browser's sign-in (browser not installed, its cookie file locked or
/// encrypted: Chrome, Edge and Brave on Windows lock theirs while open and newer ones encrypt it)
pub fn is_cookie_error(text: &str) -> bool {
    let t = text.to_lowercase();
    COOKIE_ERRORS.iter().any(|k| t.contains(k))
}

/// yt-dlp couldn't find a video at the link at all (a web page, a picture post…)
pub fn is_not_a_video_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    NOT_A_VIDEO_ERRORS.iter().any(|k| m.contains(k))
}

/// yt-dlp's error for an embedded video, said plainly for the known cases
pub fn friendly_embed_error(msg: &str, url: &str) -> String {
    let (m, u) = (msg.to_lowercase(), url.to_lowercase());
    if DEAD_VIDEO_HOSTS.iter().any(|h| u.contains(h) || m.contains(h)) {
        return "Gfycat shut down in 2023: this video can't be downloaded any more".into();
    }
    if format!("{u}{m}").contains("redgifs") && (m.contains("410") || m.contains("gone")) {
        return "RedGifs says this video was deleted (HTTP 410). If it still plays in your browser, it may only be available while \
                logged in there: pick that browser under \"Use browser sign-in\" in Settings, then Retry"
            .into();
    }
    msg.to_string()
}

pub fn referer_for(service: &str) -> &'static str {
    let s = service.to_lowercase();
    EMBED_REFERERS.iter().find(|(k, _)| *k == s).map(|(_, v)| *v).unwrap_or("")
}

/// yt-dlp's command line for one video, with progress lines on stdout
pub fn build_command(exe: &str, url: &str, folder: &str, custom_filename: Option<&str>, rate_bps: i64, referer: &str, browser: &str) -> Vec<String> {
    let out = match custom_filename.filter(|n| !n.is_empty()) {
        Some(n) => join(&[folder, &format!("{}.%(ext)s", splitext(n).0)]),
        None => join(&[folder, "%(title).100s [%(id)s].%(ext)s"]),
    };
    let mut cmd: Vec<String> = [exe, "--no-playlist", "--no-warnings", "--newline", "-o", &out, "--progress-template", PROGRESS_TEMPLATE]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if rate_bps > 0 {
        cmd.extend(["--limit-rate".into(), rate_bps.to_string()]);
    }
    if !referer.is_empty() && url.to_lowercase().contains("vimeo.com") {
        cmd.extend(["--referer".into(), referer.into()]);
    }
    if !browser.is_empty() {
        cmd.extend(["--cookies-from-browser".into(), browser.into()]);
    }
    cmd.push(url.into());
    cmd
}

/// (done bytes, total bytes, speed, time left) from a "PROGRESS:done/total:speed:eta" line
pub fn parse_progress_line(line: &str) -> Option<(i64, i64, String, String)> {
    let l = line.trim();
    let rest = l.strip_prefix("PROGRESS:")?;
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    // int(float(x)) for a count: a missing value ("NA", "") is 0; inf / nan don't make one
    let count = |s: &str| -> Option<i64> {
        if s.is_empty() || s == "NA" {
            return Some(0);
        }
        float(s).filter(|v| v.is_finite()).map(|v| v.trunc() as i64)
    };
    let bytes: Vec<&str> = parts[0].split('/').collect();
    let done = count(bytes[0])?;
    let total = match bytes.get(1) {
        Some(b) => count(b)?,
        None => 0,
    };
    let speed = if parts[1].is_empty() || parts[1] == "NA" { 0.0 } else { float(parts[1])? };
    let speed_str = if speed > 1024.0 * 1024.0 {
        format!("{:.2} MB/s", speed / (1024.0 * 1024.0))
    } else if speed > 1024.0 {
        format!("{:.1} KB/s", speed / 1024.0)
    } else if speed.is_finite() {
        format!("{} B/s", speed.trunc() as i64)
    } else {
        return None;
    };
    let eta = count(parts[2])?;
    let eta_str = if eta > 60 { format!("{}m {}s", eta / 60, eta % 60) } else { format!("{eta}s") };
    Some((done, total, speed_str, eta_str))
}

/// yt-dlp's error text as the queue shows it
pub fn short_error(full: &str) -> String {
    let msg = match full.rfind("ERROR:") {
        Some(i) => full[i + 6..].trim(),
        None => full,
    };
    msg.chars().take(120).collect()
}

// ── The program ──────────────────────────────────────────────────────────────────────────────

/// yt-dlp is there (a real download, not a cut-off one)
pub fn is_available(exe: &str) -> bool {
    std::fs::metadata(exe).map(|m| m.len() > MIN_BINARY).unwrap_or(false) || (!cfg!(windows) && which(BINARY).is_some())
}

fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
}

/// The program to run: the app's copy, else (not on Windows) the system's
fn program(exe: &str) -> String {
    if Path::new(exe).exists() || cfg!(windows) {
        return exe.to_string();
    }
    which(BINARY).unwrap_or_else(|| exe.to_string())
}

const RELEASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

/// Downloads the latest yt-dlp from GitHub into `exe`'s place (checked against the release's
/// SHA-256 list when that can be fetched)
pub fn install(exe: &str) -> bool {
    if is_available(exe) {
        return true;
    }
    let dir = dirname(exe);
    let _ = std::fs::create_dir_all(&dir);
    let tmp = format!("{exe}.{}_{}.tmp", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0));
    log("INFO", &format!("Downloading latest yt-dlp.exe from GitHub to {dir}..."));
    let result = (|| -> Result<bool, String> {
        let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(600)).connect_timeout(Duration::from_secs(60)).build().map_err(|e| e.to_string())?;
        let mut resp = client
            .get(format!("{RELEASE}{BINARY}"))
            .header("User-Agent", BROWSER_UA)
            .header("Accept", "application/octet-stream, */*")
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?;
        let total = resp.content_length().unwrap_or(0);
        let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let got = std::io::copy(&mut resp, &mut file).map_err(|e| e.to_string())?;
        drop(file);
        if total > 0 && got != total {
            log("WARNING", &format!("yt-dlp download incomplete ({got} of {total} bytes); not installed."));
            let _ = std::fs::remove_file(&tmp);
            return Ok(false);
        }
        if got <= MIN_BINARY {
            let _ = std::fs::remove_file(&tmp);
            return Ok(false);
        }
        if !checksum_ok(&client, &tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Ok(false);
        }
        std::fs::rename(&tmp, exe).or_else(|_| {
            let _ = std::fs::remove_file(exe);
            std::fs::rename(&tmp, exe)
        }).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755));
        }
        log("SUCCESS", &format!("{BINARY} successfully installed ({:.2} MB)", got as f64 / (1024.0 * 1024.0)));
        Ok(true)
    })();
    match result {
        Ok(ok) => ok,
        Err(e) => {
            if !is_available(exe) {
                log("ERROR", &format!("Failed to download yt-dlp.exe: {e}"));
            }
            let _ = std::fs::remove_file(&tmp);
            false
        }
    }
}

/// The downloaded file matches the SHA-256 yt-dlp publishes with each release (accepted when
/// the list can't be fetched: the file came over HTTPS from GitHub)
fn checksum_ok(client: &reqwest::blocking::Client, path: &str) -> bool {
    use sha2::{Digest, Sha256};
    let Ok(r) = client.get(format!("{RELEASE}SHA2-256SUMS")).timeout(Duration::from_secs(20)).send() else { return true };
    if r.status().as_u16() != 200 {
        return true;
    }
    let text = r.text().unwrap_or_default();
    let expected = text.lines().find_map(|l| {
        let p: Vec<&str> = l.split_whitespace().collect();
        (p.len() == 2 && p[1].trim_start_matches('*') == BINARY).then(|| p[0].to_lowercase())
    });
    let Some(expected) = expected else { return true };
    let Ok(mut file) = std::fs::File::open(path) else { return true };
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(_) => return true,
        }
    }
    let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != expected {
        log("ERROR", "The downloaded yt-dlp doesn't match its published checksum; it was not installed.");
        return false;
    }
    service::log_app("DEBUG", "yt-dlp checksum verified.", "ytdlp", "");
    true
}

/// Brings yt-dlp up to date (`yt-dlp -U`), or installs it when it's missing
pub fn update(exe: &str) -> bool {
    if !is_available(exe) {
        return install(exe);
    }
    if !Path::new(exe).exists() {
        // Only a system-wide yt-dlp (Linux package / pip): its package manager updates it
        log("INFO", "Using the system's yt-dlp; update it with your package manager.");
        return true;
    }
    log("INFO", "Checking for yt-dlp updates...");
    let out = run_to_end(exe, &["-U".to_string()], Duration::from_secs(45));
    match out {
        Ok((code, text)) => {
            let low = text.to_lowercase();
            if low.contains("up to date") {
                log("INFO", "yt-dlp is already up to date.");
                true
            } else if low.contains("updated yt-dlp") || code == Some(0) {
                log("SUCCESS", "yt-dlp successfully updated to latest version!");
                true
            } else {
                service::log_app("DEBUG", &format!("yt-dlp -U output: {}", text.trim()), "ytdlp", "");
                install(exe)
            }
        }
        Err(e) => {
            service::log_app("DEBUG", &format!("CLI update failed ({e}), falling back to direct download..."), "ytdlp", "");
            install(exe)
        }
    }
}

// ── Runs ─────────────────────────────────────────────────────────────────────────────────────

struct Run {
    child: std::process::Child,
    group: Option<crate::proc_group::Group>,
}

impl Run {
    fn kill(&mut self) {
        if let Some(g) = &self.group {
            g.kill();
        }
        let _ = self.child.kill();
    }
}

type Shared = Arc<Mutex<Run>>;

fn running() -> &'static Mutex<HashMap<u64, Shared>> {
    static R: OnceLock<Mutex<HashMap<u64, Shared>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Stops every yt-dlp that's running
pub fn cancel_all() {
    for (_, r) in running().lock().unwrap().drain() {
        r.lock().unwrap().kill();
    }
}

fn spawn(exe: &str, args: &[String]) -> std::io::Result<Shared> {
    let mut cmd = Command::new(exe);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | crate::proc_group::CREATE_SUSPENDED); // (no console window; started once it's in its job)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    let group = crate::proc_group::adopt(&child);
    Ok(Arc::new(Mutex::new(Run { child, group })))
}

/// Runs to the end (or `limit`): (exit code, output and errors)
fn run_to_end(exe: &str, args: &[String], limit: Duration) -> Result<(Option<i32>, String), String> {
    let run = spawn(exe, args).map_err(|e| e.to_string())?;
    let (out, err) = {
        let mut r = run.lock().unwrap();
        (r.child.stdout.take(), r.child.stderr.take())
    };
    let read = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut s);
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let o = read(out.map(|p| Box::new(p) as Box<dyn Read + Send>));
    let e = read(err.map(|p| Box::new(p) as Box<dyn Read + Send>));
    let end = Instant::now() + limit;
    let code = loop {
        if let Ok(Some(st)) = run.lock().unwrap().child.try_wait() {
            break st.code();
        }
        if Instant::now() > end {
            run.lock().unwrap().kill();
            return Err("timed out".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Ok((code, format!("{}{}", o.join().unwrap_or_default(), e.join().unwrap_or_default())))
}

/// One yt-dlp run: (ok, message, yt-dlp's whole error text)
fn run_once(exe: &str, args: &[String], ctl: &Ctl, progress: &mut dyn FnMut(i64, i64, String, String)) -> (bool, String, String) {
    static IDS: AtomicU64 = AtomicU64::new(1);
    let run = match spawn(exe, args) {
        Ok(r) => r,
        Err(e) => return (false, e.to_string(), e.to_string()),
    };
    let id = IDS.fetch_add(1, Ordering::Relaxed);
    running().lock().unwrap().insert(id, run.clone());
    let (out, err) = {
        let mut r = run.lock().unwrap();
        (r.child.stdout.take(), r.child.stderr.take())
    };
    // Its output a line at a time; while paused nothing is read, so yt-dlp waits once the pipe is full
    let (tx, rx) = sync_channel::<String>(8);
    let reader = std::thread::spawn(move || {
        let mut other = String::new();
        if let Some(out) = out {
            for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
                let line = String::from_utf8_lossy(&line).trim_end_matches('\r').to_string();
                if line.trim().starts_with("PROGRESS:") {
                    if tx.send(line).is_err() {
                        break;
                    }
                } else if !line.trim().is_empty() {
                    other.push_str(&line);
                    other.push('\n');
                }
            }
        }
        other
    });
    let errs = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(mut e) = err {
            let _ = e.read_to_end(&mut s);
        }
        String::from_utf8_lossy(&s).into_owned()
    });
    let stop = |run: &Shared| {
        run.lock().unwrap().kill();
        running().lock().unwrap().remove(&id);
        (false, "Cancelled".to_string(), String::new())
    };
    loop {
        if ctl.cancelled() {
            return stop(&run);
        }
        if ctl.paused() {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if let Some((d, t, s, e)) = parse_progress_line(&line) {
                    progress(d, t, s, e);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let code = loop {
        if ctl.cancelled() {
            return stop(&run);
        }
        match run.lock().unwrap().child.try_wait() {
            Ok(Some(st)) => break st.code(),
            Ok(None) => {}
            Err(e) => {
                running().lock().unwrap().remove(&id);
                return (false, e.to_string(), e.to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let was_running = running().lock().unwrap().remove(&id).is_some();
    let other = reader.join().unwrap_or_default();
    let stderr = errs.join().unwrap_or_default();
    if !was_running || ctl.cancelled() {
        // (stopped by cancel_all)
        return (false, "Cancelled".into(), String::new());
    }
    if code == Some(0) {
        return (true, "Completed".into(), String::new());
    }
    let full = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else if !other.trim().is_empty() {
        other.trim().to_string()
    } else {
        format!("yt-dlp exited with code {}", code.map(|c| c.to_string()).unwrap_or_else(|| "None".into()))
    };
    (false, short_error(&full), full)
}

/// Browsers whose sign-in couldn't be read (tried once a session)
fn cookies_failed() -> &'static Mutex<String> {
    static F: OnceLock<Mutex<String>> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// Downloads one video into `folder` (named after `custom_filename` when given), with the browser
/// sign-in when one is set and it can be read: (ok, message)
#[allow(clippy::too_many_arguments)]
pub fn download_media(exe: &str, browser: &str, url: &str, folder: &str, custom_filename: Option<&str>, referer: &str, rate_bps: i64, ctl: &Ctl,
                      progress: &mut dyn FnMut(i64, i64, String, String)) -> (bool, String) {
    if ctl.cancelled() {
        return (false, "Cancelled".into());
    }
    if !is_available(exe) && (!install(exe) || !is_available(exe)) {
        return (false, "yt-dlp.exe is missing and could not be downloaded.".into());
    }
    let _ = std::fs::create_dir_all(folder);
    let prog = program(exe);
    let browser = if BROWSERS.contains(&browser) { browser } else { "" };
    if !browser.is_empty() && *cookies_failed().lock().unwrap() != browser {
        let cmd = build_command(&prog, url, folder, custom_filename, rate_bps, referer, browser);
        let (ok, msg, raw) = run_once(&prog, &cmd[1..], ctl, progress);
        if ok || !is_cookie_error(&raw) {
            return (ok, msg);
        }
        // Not the video's fault: download without the sign-in (and stop trying it this session)
        *cookies_failed().lock().unwrap() = browser.to_string();
        let mut name = browser.to_string();
        name[..1].make_ascii_uppercase();
        log("WARNING", &format!("Couldn't use {name}'s sign-in for embedded videos ({msg}). Downloading them without it. Firefox works best; \
                                 Chrome, Edge and Brave have to be closed, and their newest versions can't be read at all."));
    }
    let cmd = build_command(&prog, url, folder, custom_filename, rate_bps, referer, "");
    let (ok, msg, _) = run_once(&prog, &cmd[1..], ctl, progress);
    (ok, msg)
}

/// One embedded video of the queue (FileDownloader's yt-dlp branch): progress, the size filter
/// once it's there, and "not a video link" skipped
pub fn fetch(c: &Cell, o: &Options, ctl: &Ctl, hooks: &dyn Hooks, exe: &str, browser: &str) -> (bool, String) {
    let task = c.lock().unwrap().task.clone();
    let filename = basename(&task.target_path).to_string();
    log("INFO", &format!("▶ [yt-dlp] Downloading embedded player media: {}", task.url));
    let set = |f: &dyn Fn(&mut super::fetch::Item)| {
        f(&mut c.lock().unwrap());
        hooks.changed(c);
    };
    let mut prev: i64 = 0;
    let mut on_progress = |done: i64, total: i64, speed: String, eta: String| {
        if done < prev {
            prev = 0; // the next stream (a video's sound comes after its picture)
        }
        if done > prev {
            ctl.bytes.fetch_add((done - prev) as u64, Ordering::Relaxed);
            prev = done;
        }
        let mut it = c.lock().unwrap();
        it.st.downloaded_bytes = done.max(0) as u64;
        if total != 0 {
            it.task.file_size = total;
        }
        it.st.speed_str = speed;
        it.st.eta_str = eta;
        if total > 0 {
            it.st.progress_pct = (done as f64 / total as f64 * 100.0).trunc().clamp(0.0, 99.0) as u32;
        }
        drop(it);
        hooks.changed(c);
    };
    let rate = hooks.limit_bps() as i64;
    let (ok, msg) = download_media(exe, browser, &task.url, &dirname(&task.target_path), Some(&filename), referer_for(&task.service), rate, ctl, &mut on_progress);
    if !ok && is_not_a_video_error(&msg) {
        log("INFO", &format!("⏭️ [yt-dlp] Skipping {}: not a video link", task.url));
        set(&|it| {
            it.st.status = "skipped".into();
            it.st.error_msg = "Not a video link".into();
            it.st.progress_pct = 100;
            it.st.eta_str = "Skipped".into();
        });
        return (true, "Skipped: not a video link".into());
    }
    if !ok {
        let msg = friendly_embed_error(&msg, &task.url);
        log("WARNING", &format!("✖ [yt-dlp] {filename}: {msg}"));
        return (false, msg);
    }
    // yt-dlp picks the extension (a YouTube video with its sound is often .webm or .mkv): the
    // queue gets the file's real name
    let found = if Path::new(&task.target_path).exists() { None } else { saved_as(&task.target_path) };
    let (task, filename) = match found {
        Some(path) => {
            c.lock().unwrap().task.target_path = path.clone();
            let name = basename(&path).to_string();
            (super::plan::Task { target_path: path, ..task }, name)
        }
        None => (task, filename),
    };
    let size = std::fs::metadata(&task.target_path).map(|m| m.len()).unwrap_or(0);
    if size > 0 {
        let (keep, why) = f::should_keep_file(&filename, o, Some(size as i64));
        if !keep {
            let _ = std::fs::remove_file(&task.target_path);
            let had = c.lock().unwrap().st.downloaded_bytes;
            let _ = ctl.bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |b| Some(b.saturating_sub(had)));
            service::log_app("INFO", &format!("⏭️ Skipping [yt-dlp] {filename} ({}): {why}", f::format_size_str(size as i64)), "filter", "");
            set(&|it| {
                it.st.status = "skipped".into();
                it.st.error_msg = why.clone();
                it.st.progress_pct = 100;
                it.st.eta_str = "Skipped".into();
            });
            return (true, format!("Skipped: {why}"));
        }
    }
    set(&|it| {
        it.st.status = "completed".into();
        if size > 0 {
            it.st.downloaded_bytes = size;
            it.task.file_size = size as i64;
        }
        it.st.progress_pct = 100;
        it.st.eta_str = "Done".into();
    });
    log("SUCCESS", &format!("✔ [yt-dlp] {filename} successfully downloaded"));
    (true, "Completed".into())
}

/// The file yt-dlp saved for `target` when it chose another extension ("clip.mp4" → "clip.webm")
fn saved_as(target: &str) -> Option<String> {
    let stem = splitext(basename(target)).0.to_string();
    let dir = dirname(target);
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for e in std::fs::read_dir(if dir.is_empty() { "." } else { &dir }).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let (s, ext) = splitext(&name);
        let ext = ext.to_lowercase();
        // (not yt-dlp's unfinished or per-stream files: "clip.f401.mp4", ".part", ".ytdl")
        if s != stem || ext.is_empty() || [".part", ".ytdl", ".temp", ".tmp"].contains(&ext.as_str()) || !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let when = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(w, _)| when > *w) {
            best = Some((when, e.path().to_string_lossy().into_owned()));
        }
    }
    best.map(|(_, p)| p)
}

/// The app's requests: {"do": "update", "exe"} (in the background at start-up), {"do": "retry_sign_in"}, {"do": "cancel_all"}
pub fn call(args: &Value) -> Result<Value, String> {
    match args["do"].as_str().unwrap_or("") {
        "update" => Ok(json!(update(args["exe"].as_str().unwrap_or("")))),
        "install" => Ok(json!(install(args["exe"].as_str().unwrap_or("")))),
        // (a browser picked in Settings: its sign-in is tried again)
        "retry_sign_in" => {
            cookies_failed().lock().unwrap().clear();
            Ok(Value::Null)
        }
        "cancel_all" => {
            cancel_all();
            Ok(Value::Null)
        }
        other => Err(format!("ytdlp: unknown request {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/ytdlp.json")).unwrap()
    }

    #[test]
    fn matches_python() {
        let fx = fixture();
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        for case in fx["commands"].as_array().unwrap() {
            let a = case[0].as_array().unwrap();
            let custom = a[3].as_str();
            let got = build_command(&s(&a[0]), &s(&a[1]), &s(&a[2]), custom, a[4].as_i64().unwrap(), &s(&a[5]), &s(&a[6]));
            assert_eq!(json!(got), case[1], "command {a:?}");
        }
        for case in fx["progress"].as_array().unwrap() {
            let got = parse_progress_line(case[0].as_str().unwrap()).map(|(a, b, c, d)| json!([a, b, c, d])).unwrap_or(Value::Null);
            assert_eq!(got, case[1], "progress {:?}", case[0]);
        }
        for case in fx["errors"].as_array().unwrap() {
            assert_eq!(json!(short_error(case[0].as_str().unwrap())), case[1], "error {:?}", case[0]);
        }
        for case in fx["cookies"].as_array().unwrap() {
            assert_eq!(json!(is_cookie_error(case[0].as_str().unwrap())), case[1], "cookies {:?}", case[0]);
        }
        for case in fx["messages"].as_array().unwrap() {
            let (m, u) = (case[0].as_str().unwrap(), case[1].as_str().unwrap());
            assert_eq!(json!(is_not_a_video_error(m)), case[2], "not a video {m:?}");
            assert_eq!(json!(friendly_embed_error(m, u)), case[3], "message {m:?}");
        }
        assert_eq!(json!(BROWSERS), fx["browsers"]);
        let refs: serde_json::Map<String, Value> = EMBED_REFERERS.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        assert_eq!(Value::Object(refs), fx["referers"]);
    }
}

/// A real download with the app's yt-dlp: YTDLP_TEST_URL=<video link> cargo test real_download -- --ignored
#[cfg(test)]
mod live {
    use super::*;
    use crate::downloader::fetch::{cell, Hooks};
    use crate::downloader::plan::Task;
    use std::sync::atomic::AtomicUsize;

    struct Count(AtomicUsize);
    impl Hooks for Count {
        fn changed(&self, _c: &Cell) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    #[ignore]
    fn real_download() {
        let url = std::env::var("YTDLP_TEST_URL").expect("YTDLP_TEST_URL");
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let exe = format!("{root}/dependencies/yt-dlp.exe");
        let target = format!("{}/target/queue_test/ytdlp/clip.mp4", env!("CARGO_MANIFEST_DIR"));
        let _ = std::fs::remove_file(&target);
        let c = cell(Task { url, target_path: target.clone(), service: "patreon".into(), is_ytdlp: true, ..Default::default() });
        let hooks = Count(AtomicUsize::new(0));
        let ctl = Ctl::default();
        let (ok, msg) = fetch(&c, &Options::from_dict(&json!({})), &ctl, &hooks, &exe, "");
        let it = c.lock().unwrap();
        let saved = it.task.target_path.clone();
        println!("{ok} {msg:?} status={} pct={} bytes={} run_bytes={} updates={} saved={saved} size={:?}", it.st.status, it.st.progress_pct, it.st.downloaded_bytes,
                 ctl.bytes.load(Ordering::Relaxed), hooks.0.load(Ordering::Relaxed), std::fs::metadata(&saved).map(|m| m.len()));
        assert!(ok && it.st.status == "completed" && Path::new(&saved).exists() && hooks.0.load(Ordering::Relaxed) > 2);
        let _ = std::fs::remove_file(&saved);

        // Stopped part way: "Cancelled", and no yt-dlp left running
        drop(it);
        let _ = std::fs::remove_file(&target);
        let c = cell(Task { url: std::env::var("YTDLP_TEST_URL").unwrap(), target_path: target.clone(), is_ytdlp: true, ..Default::default() });
        let ctl = Arc::new(Ctl::default());
        let stopper = {
            let ctl = ctl.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(2500));
                ctl.cancel.store(true, Ordering::Relaxed);
            })
        };
        let t0 = Instant::now();
        let (ok, msg) = fetch(&c, &Options::from_dict(&json!({})), &ctl, &hooks, &exe, "");
        stopper.join().unwrap();
        println!("cancel: {ok} {msg:?} after {:?}", t0.elapsed());
        assert!(!ok && msg == "Cancelled");
        assert!(running().lock().unwrap().is_empty());
    }
}
