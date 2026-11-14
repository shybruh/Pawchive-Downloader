//! The download engine (fetch.rs, queue.rs) against a local test server: mirrors, resuming,
//! 404 / 429 / 500 answers, size filters, pause / resume / cancel, retries and a full queue run.

use super::fetch::{self, Cell, Ctl, Hooks};
use super::filters::Options;
use super::plan::Task;
use super::queue::{Queue, QueueHooks};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The test server: /file/<n>?size=<bytes>[&slow=1] serves deterministic bytes (with ranges),
/// /status/<code>, /flaky/<key>?fail=<n>&code=<c> fails n times then serves a file.
fn server() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        let s = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", s.server_addr().to_ip().unwrap().port());
        let s = Arc::new(s);
        let counts: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        for _ in 0..8 {
            let (s, counts) = (s.clone(), counts.clone());
            std::thread::spawn(move || {
                for req in s.incoming_requests() {
                    handle(req, &counts);
                }
            });
        }
        url
    })
}

fn body(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i * 31 % 251) as u8).collect()
}

fn handle(req: tiny_http::Request, counts: &Mutex<HashMap<String, usize>>) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let q: HashMap<&str, &str> = query.split('&').filter_map(|kv| kv.split_once('=')).collect();
    let range = req.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.to_string());
    if q.contains_key("slow") {
        return slow_file(req, &q, range.as_deref());
    }
    let respond = |code: u16, data: Vec<u8>, extra: Vec<(&str, String)>| {
        let mut r = tiny_http::Response::from_data(data).with_status_code(code);
        for (k, v) in extra {
            r.add_header(tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap());
        }
        let _ = req.respond(r);
    };
    if let Some(code) = path.strip_prefix("/status/") {
        let code: u16 = code.parse().unwrap();
        return respond(code, b"no".to_vec(), if code == 429 { vec![("Retry-After", "1".into())] } else { vec![] });
    }
    if let Some(key) = path.strip_prefix("/flaky/") {
        let n = {
            let mut c = counts.lock().unwrap();
            let e = c.entry(key.to_string()).or_insert(0);
            *e += 1;
            *e
        };
        if n <= q.get("fail").and_then(|f| f.parse().ok()).unwrap_or(0) {
            return respond(q.get("code").and_then(|c| c.parse().ok()).unwrap_or(500), b"busy".to_vec(), vec![]);
        }
    }
    let size: usize = q.get("size").and_then(|s| s.parse().ok()).unwrap_or(1000);
    let data = body(size);
    let (start, code) = match range.as_deref().and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split('-').next()).and_then(|s| s.parse::<usize>().ok()) {
        Some(s) if s >= size => return respond(416, vec![], vec![("Content-Range", format!("bytes */{size}"))]),
        Some(s) => (s, 206),
        None => (0, 200),
    };
    let mut headers = vec![("Accept-Ranges", "bytes".to_string())];
    if code == 206 {
        headers.push(("Content-Range", format!("bytes {start}-{}/{size}", size - 1)));
    }
    respond(code, data[start..].to_vec(), headers);
}


fn slow_file(req: tiny_http::Request, q: &HashMap<&str, &str>, range: Option<&str>) {
    let size: usize = q.get("size").and_then(|s| s.parse().ok()).unwrap_or(1000);
    let data = body(size);
    let start = range.and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split('-').next()).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
    let code = if start > 0 { 206 } else { 200 };
    let mut headers = vec![("Accept-Ranges", "bytes".to_string())];
    if code == 206 {
        headers.push(("Content-Range", format!("bytes {start}-{}/{size}", size - 1)));
    }
        // About 1 MB/s, so a test can pause in the middle
    struct Slow(Vec<u8>, usize);
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(20));
            let n = buf.len().min(20_000).min(self.0.len() - self.1);
            buf[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
            self.1 += n;
            Ok(n)
        }
    }
    let len = size - start;
    let mut r = tiny_http::Response::new(code.into(), vec![], Slow(data[start..].to_vec(), 0), Some(len), None);
    for (k, v) in headers {
        r.add_header(tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap());
    }
    let _ = req.respond(r);
    return;
}

fn dir(name: &str) -> PathBuf {
    // (one folder per test run: runs at the same time don't empty each other's; earlier runs'
    // are removed once they're ten minutes old)
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("engine_tests");
    for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > Duration::from_secs(600));
        let fname = e.file_name().to_string_lossy().into_owned();
        if old && (fname == name || fname.starts_with(&format!("{name}_"))) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let d = root.join(format!("{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn task(d: &PathBuf, name: &str, urls: &[String]) -> Task {
    Task {
        url: urls[0].clone(),
        fallback_urls: urls[1..].to_vec(),
        target_path: d.join(name).to_string_lossy().into_owned(),
        post_id: "1".into(),
        file_id: format!("1_{name}"),
        batch_id: "b".into(),
        service: "patreon".into(),
        ..Default::default()
    }
}

fn opts(extra: Value) -> Options {
    let mut d = json!({"download_delay": 0});
    d.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    Options::from_dict(&d)
}

#[derive(Default)]
struct Rec {
    finished: Mutex<Option<(bool, String)>>,
    progress: AtomicUsize,
}
impl Hooks for Rec {}
impl QueueHooks for Rec {
    fn progress(&self, _: &Value) {
        self.progress.fetch_add(1, Ordering::Relaxed);
    }
    fn finished(&self, ok: bool, s: &str) {
        *self.finished.lock().unwrap() = Some((ok, s.to_string()));
    }
}

fn status(c: &Cell) -> String {
    c.lock().unwrap().st.status.clone()
}

fn read(p: &str) -> Vec<u8> {
    std::fs::read(p).unwrap_or_default()
}

#[test]
fn mirrors_resume_and_skips() {
    let s = server();
    let d = dir("mirrors");
    let ctl = Ctl::default();
    let o = opts(json!({}));

    // The first server hasn't got it, the second has
    let c = fetch::cell(task(&d, "a.bin", &[format!("{s}/status/404"), format!("{s}/file/a?size=300000")]));
    let r = fetch::download(&c, &o, &Rec::default(), &ctl);
    assert_eq!(r, (true, "Success".into()), "{:?}", c.lock().unwrap().st);
    let p = c.lock().unwrap().task.target_path.clone();
    assert_eq!(read(&p), body(300000));
    assert!(!std::path::Path::new(&format!("{p}.part")).exists());
    assert_eq!(c.lock().unwrap().task.url, format!("{s}/file/a?size=300000"));

    // Already there
    let c2 = fetch::cell(task(&d, "a.bin", &[format!("{s}/file/a?size=300000")]));
    assert_eq!(fetch::download(&c2, &o, &Rec::default(), &ctl), (true, "Already downloaded".into()));
    assert_eq!(status(&c2), "completed");

    // An interrupted download continues from its .part file (asks for the rest with a range)
    let t = task(&d, "b.bin", &[format!("{s}/file/b?size=200000")]);
    std::fs::write(format!("{}.part", t.target_path), &body(200000)[..50000]).unwrap();
    let c3 = fetch::cell(t);
    assert_eq!(fetch::download(&c3, &o, &Rec::default(), &ctl).0, true);
    assert_eq!(read(&c3.lock().unwrap().task.target_path), body(200000));

    // A .part file that already holds everything (the server answers 416)
    let t = task(&d, "c.bin", &[format!("{s}/file/c?size=5000")]);
    std::fs::write(format!("{}.part", t.target_path), body(5000)).unwrap();
    let c4 = fetch::cell(Task { file_size: 5000, ..t });
    assert_eq!(fetch::download(&c4, &o, &Rec::default(), &ctl), (true, "Success".into()));
    assert_eq!(read(&c4.lock().unwrap().task.target_path), body(5000));

    // Too big for the size filter: skipped once the server tells the size
    let c5 = fetch::cell(task(&d, "big.bin", &[format!("{s}/file/big?size=900000")]));
    let r = fetch::download(&c5, &opts(json!({"max_file_size": "100 KB"})), &Rec::default(), &ctl);
    assert!(r.0 && r.1.starts_with("Skipped:"), "{r:?}");
    assert!(!std::path::Path::new(&c5.lock().unwrap().task.target_path).exists());

    // Missing everywhere, and no server answering at all
    let c6 = fetch::cell(task(&d, "gone.bin", &[format!("{s}/status/404")]));
    assert_eq!(fetch::download(&c6, &o, &Rec::default(), &ctl), (false, "404 Not Found — file does not exist on server".into()));
    let c7 = fetch::cell(task(&d, "x.bin", &["http://127.0.0.1:9/nothing".to_string()]));
    assert_eq!(fetch::download(&c7, &o, &Rec::default(), &ctl), (false, "Failed to connect to any file server".into()));

    // Busy (429): reported for the queue's cooldown
    let c8 = fetch::cell(task(&d, "busy.bin", &[format!("{s}/status/429")]));
    let r = fetch::download(&c8, &o, &Rec::default(), &ctl);
    assert!(!r.0 && r.1.starts_with("429_RATE_LIMIT"), "{r:?}");

    // A preview-only link is skipped unless thumbnails are allowed
    let c9 = fetch::cell(task(&d, "p.jpg", &["https://img.pawchive.pw/thumbnail/data/ab/cd/x.jpg".to_string()]));
    assert_eq!(fetch::download(&c9, &o, &Rec::default(), &ctl).1, "Preview thumbnail skipped (fallback to thumbnails is disabled)");
}

#[test]
fn pause_resume_and_cancel() {
    // (no fixed waits: on a busy machine the first bytes, or the pause, come later)
    let bytes = |c: &fetch::Cell| c.lock().unwrap().st.downloaded_bytes;
    let started_downloading = |c: &fetch::Cell| {
        let end = Instant::now() + Duration::from_secs(20);
        while bytes(c) == 0 {
            assert!(Instant::now() < end, "the download didn't start in 20s");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let s = server();
    let d = dir("pause");
    let o = Arc::new(opts(json!({})));
    let ctl = Arc::new(Ctl::default());
    let c = fetch::cell(task(&d, "slow.bin", &[format!("{s}/file/slow?size=3000000&slow=1")]));
    let (c2, ctl2, o2) = (c.clone(), ctl.clone(), o.clone());
    let worker = std::thread::spawn(move || fetch::download(&c2, &o2, &Rec::default(), &ctl2));
    started_downloading(&c);
    ctl.pause.store(true, Ordering::Relaxed);
    // Paused once the count stops moving (what was already read in still lands)
    let end = Instant::now() + Duration::from_secs(10);
    let mut paused_at = bytes(&c);
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let now = bytes(&c);
        if now == paused_at {
            break;
        }
        assert!(Instant::now() < end, "still downloading 10s after the pause ({now} bytes)");
        paused_at = now;
    }
    assert!(paused_at > 0 && paused_at < 3000000, "paused at {paused_at}");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(bytes(&c), paused_at, "nothing downloads while paused");
    ctl.pause.store(false, Ordering::Relaxed);
    let r = worker.join().unwrap();
    assert_eq!(r, (true, "Success".into()));
    assert_eq!(read(&c.lock().unwrap().task.target_path), body(3000000));

    // Cancel: stops, keeps what's on disk for the next try
    let ctl = Arc::new(Ctl::default());
    let c = fetch::cell(task(&d, "slow2.bin", &[format!("{s}/file/slow2?size=3000000&slow=1")]));
    let (c2, ctl2) = (c.clone(), ctl.clone());
    let worker = std::thread::spawn(move || fetch::download(&c2, &o, &Rec::default(), &ctl2));
    started_downloading(&c);
    ctl.cancel.store(true, Ordering::Relaxed);
    let started = Instant::now();
    assert_eq!(worker.join().unwrap(), (false, "Cancelled".into()));
    // (the whole file takes about 3s: a cancel that waited for it would take longer than this)
    assert!(started.elapsed() < Duration::from_secs(5), "cancel took {:?}", started.elapsed());
    let part = format!("{}.part", c.lock().unwrap().task.target_path);
    assert!(std::path::Path::new(&part).exists());

    // One file cancelled while it downloads (its post's "Cancel remaining"): it stops too
    let ctl = Arc::new(Ctl::default());
    let c = fetch::cell(task(&d, "slow3.bin", &[format!("{s}/file/slow3?size=3000000&slow=1")]));
    let (c2, ctl2, o2) = (c.clone(), ctl.clone(), Arc::new(opts(json!({}))));
    let worker = std::thread::spawn(move || fetch::download(&c2, &o2, &Rec::default(), &ctl2));
    started_downloading(&c);
    c.lock().unwrap().st.status = "cancelled".into();
    let started = Instant::now();
    assert_eq!(worker.join().unwrap(), (false, "Cancelled".into()));
    assert!(started.elapsed() < Duration::from_secs(5), "cancel took {:?}", started.elapsed());
}

fn wait_finished(rec: &Rec, secs: u64) -> (bool, String) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Some(f) = rec.finished.lock().unwrap().clone() {
            return f;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the queue didn't finish in {secs}s");
}

#[test]
fn queue_run_with_retries() {
    let s = server();
    let d = dir("queue");
    let rec = Arc::new(Rec::default());
    let q = Queue::new(rec.clone(), 3);
    let mut tasks: Vec<Task> = (0..6).map(|i| task(&d, &format!("f{i}.bin"), &[format!("{s}/file/q{i}?size={}", 10000 * (i + 1))])).collect();
    // Fails with a server error (both passes over its servers), then works: the automatic retry at the end gets it
    tasks.push(task(&d, "flaky.bin", &[format!("{s}/flaky/q1?fail=2&code=500&size=4000")]));
    tasks.push(task(&d, "missing.bin", &[format!("{s}/status/404")]));
    let o = Arc::new(opts(json!({"auto_retry_at_end": true, "skip_retry_404": true})));
    assert_eq!(q.append(tasks.clone(), Some(o.clone())), 8);
    // The same files again are not added twice
    assert_eq!(q.append(tasks, Some(o.clone())), 0);

    let (ok, summary) = wait_finished(&rec, 60);
    assert!(ok, "{summary}");
    let cells = q.cells();
    let by = |n: &str| cells.iter().find(|c| c.lock().unwrap().task.target_path.ends_with(n)).unwrap().clone();
    for i in 0..6 {
        let c = by(&format!("f{i}.bin"));
        assert_eq!(status(&c), "completed", "{:?}", c.lock().unwrap().st);
        assert_eq!(read(&c.lock().unwrap().task.target_path), body(10000 * (i + 1)));
    }
    let flaky = by("flaky.bin");
    assert_eq!(status(&flaky), "completed", "{:?}", flaky.lock().unwrap().st);
    assert_eq!(flaky.lock().unwrap().st.retry_count, 1);
    assert_eq!(status(&by("missing.bin")), "failed");
    assert!(summary.contains("7 downloaded") && summary.contains("1 failed"), "{summary}");
    assert!(rec.progress.load(Ordering::Relaxed) > 0);
    let info = q.progress_info();
    assert_eq!(info["completed"], 7);
    assert_eq!(info["total"], 8);

    // Two different files after the same path: the second is renamed
    let rec2 = Arc::new(Rec::default());
    let q2 = Queue::new(rec2.clone(), 2);
    let a = task(&d, "same.bin", &[format!("{s}/file/s1?size=100")]);
    let b = Task { url: format!("{s}/file/s2?size=200"), file_id: "other".into(), ..a.clone() };
    q2.append(vec![a, b], Some(o.clone()));
    wait_finished(&rec2, 30);
    assert_eq!(read(&d.join("same.bin").to_string_lossy()), body(100));
    assert_eq!(read(&d.join("same (2).bin").to_string_lossy()), body(200));

    // Pausing a batch stops its files from starting; resuming finishes them
    let rec3 = Arc::new(Rec::default());
    let q3 = Queue::new(rec3.clone(), 1);
    q3.pause_batch("b");
    let t = task(&d, "later.bin", &[format!("{s}/file/later?size=1000")]);
    q3.append(vec![t], Some(o.clone()));
    std::thread::sleep(Duration::from_millis(1500));
    assert!(rec3.finished.lock().unwrap().is_none());
    assert_eq!(status(&q3.cells()[0]), "pending");
    assert_eq!(q3.resume_batch("b", None), 0); // nothing was paused yet, only the batch
    wait_finished(&rec3, 30);
    assert_eq!(status(&q3.cells()[0]), "completed");
}

/// The example post from Pawchive, planned and downloaded with the Rust pieces only
/// (cargo test --lib -- --ignored live_download)
#[test]
#[ignore]
fn live_download() {
    use super::plan::{build, NoLookups, Request};
    use super::sites::SiteClient;
    let d = dir("live");
    let client = SiteClient::new("", Box::new(|_| String::new()));
    let post = client.single_post(&super::links::parse("https://pawchive.pw/patreon/user/152819670/post/174018597")).expect("the post");
    let o = Arc::new(opts(json!({"exact_extensions": "gif", "save_post_metadata": false})));
    let base = d.to_string_lossy().into_owned();
    let r = Request { creator_name: "SillyTeshii", service: "patreon", domain: "pawchive.pw", base_dir: &base, batch_id: None, artist_dir: None, user_id: "152819670", include_service: true };
    let plan = build(&[post], &o, &r, &NoLookups);
    assert!(!plan.tasks.is_empty(), "nothing planned");
    let rec = Arc::new(Rec::default());
    let q = Queue::new(rec.clone(), 2);
    q.append(plan.tasks.clone(), Some(o));
    let (ok, summary) = wait_finished(&rec, 600);
    assert!(ok, "{summary}");
    for c in q.cells() {
        let it = c.lock().unwrap();
        assert_eq!(it.st.status, "completed", "{} {:?}", it.task.target_path, it.st);
        let hash = fetch::content_hash_from_url(&it.task.url);
        assert_eq!(hash.len(), 64, "a Pawchive file with its hash in the name: {}", it.task.url);
        use sha2::Digest;
        let got: String = sha2::Sha256::digest(read(&it.task.target_path)).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, hash, "{}", it.task.target_path);
        eprintln!("ok {} ({} bytes)", it.task.target_path, it.task.file_size);
    }
}
