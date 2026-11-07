//! Downloading one file into its ".part" file, in chunks that survive interruptions.
//!
//! The file is split into fixed 8 MB chunks. Each finished chunk is recorded in a journal next to
//! the ".part" file (`<part>.chunks`) together with its CRC-32, so after a crash, a dropped
//! connection or a pause the download continues from the chunks that are verifiably on disk; a
//! chunk whose bytes don't match its record (a torn write, a damaged disk) is fetched again.
//!
//! One connection streams the missing stretches in order (what Pawchive asks for). Several
//! connections each fetch missing chunks with HTTP Range requests (for hosts that allow it). A
//! connection that drops retries from the last byte it wrote, with back-off. Sites only publish a
//! hash of the whole file, so the caller still checks the finished file's SHA-256.

use super::limiter;
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_RANGE, RANGE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub const CHUNK: u64 = 8 * 1024 * 1024;
const BUF: usize = 256 * 1024;
const MAX_ATTEMPTS: u32 = 6;

#[derive(Deserialize)]
pub struct Job {
    pub url: String,
    pub part_path: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// The file's size when known (0 = unknown: streamed in one go, no journal)
    #[serde(default)]
    pub total: u64,
    /// Whether the server answers byte ranges
    #[serde(default)]
    pub ranges: bool,
    #[serde(default = "one")]
    pub connections: usize,
    /// Throw away what's there and start over
    #[serde(default)]
    pub restart: bool,
    /// Compute the finished file's SHA-256
    #[serde(default)]
    pub hash: bool,
    /// Download speed limit for all transfers together, bytes per second (0 = none)
    #[serde(default)]
    pub limit_bps: f64,
}

fn one() -> usize {
    1
}

pub struct Done {
    pub size: u64,
    pub sha256: String,
    /// Bytes that were already on disk when this run started
    pub resumed_from: u64,
}

#[derive(Debug)]
pub enum Fail {
    Cancelled,
    Http(u16, String),
    Network(String),
    Disk(String),
    Other(String),
}

impl Fail {
    pub fn kind(&self) -> &'static str {
        match self {
            Fail::Cancelled => "cancelled",
            Fail::Http(..) => "http",
            Fail::Network(_) => "network",
            Fail::Disk(_) => "disk",
            Fail::Other(_) => "other",
        }
    }
    pub fn message(&self) -> String {
        match self {
            Fail::Cancelled => "Cancelled".into(),
            Fail::Http(code, m) => if m.is_empty() { format!("HTTP {code}") } else { m.clone() },
            Fail::Network(m) | Fail::Disk(m) | Fail::Other(m) => m.clone(),
        }
    }
    pub fn status(&self) -> u16 {
        if let Fail::Http(c, _) = self { *c } else { 0 }
    }
}

fn io_fail(e: std::io::Error) -> Fail {
    let full = matches!(e.raw_os_error(), Some(112) | Some(28) | Some(39)) || e.to_string().to_lowercase().contains("space");
    if full {
        Fail::Disk("Disk full: not enough free space on the drive".into())
    } else {
        Fail::Disk(format!("Disk/IO error: {e}"))
    }
}

/// Shared state of one transfer, read by the progress reporter.
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

fn client() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(25))
            .timeout(Duration::from_secs(60)) // per read: a stalled connection gives up
            .pool_max_idle_per_host(8)
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .expect("HTTP client")
    })
}

// ── The journal ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct Journal {
    total: u64,
    chunk: u64,
    /// CRC-32 of each finished chunk (absent = not finished)
    done: HashMap<u64, u32>,
}

struct Book {
    /// the journal file
    path: PathBuf,
    /// the ".part" file it describes
    part: PathBuf,
    journal: Mutex<Journal>,
}

impl Book {
    fn chunks(total: u64) -> u64 {
        total.div_ceil(CHUNK)
    }
    fn span(&self, i: u64) -> (u64, u64) {
        let total = self.journal.lock().unwrap().total;
        (i * CHUNK, ((i + 1) * CHUNK).min(total))
    }
    fn is_done(&self, i: u64) -> bool {
        self.journal.lock().unwrap().done.contains_key(&i)
    }
    fn finish(&self, i: u64, crc: u32) {
        let mut j = self.journal.lock().unwrap();
        j.done.insert(i, crc);
        if let Ok(text) = serde_json::to_vec(&*j) {
            let tmp = self.path.with_extension("chunks.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
    fn done_bytes(&self) -> u64 {
        let j = self.journal.lock().unwrap();
        j.done.keys().map(|i| ((i + 1) * CHUNK).min(j.total) - i * CHUNK).sum()
    }
}

fn crc_of_region(file: &mut File, start: u64, end: u64) -> std::io::Result<u32> {
    file.seek(SeekFrom::Start(start))?;
    let mut h = crc32fast::Hasher::new();
    let mut left = end - start;
    let mut buf = vec![0u8; BUF];
    while left > 0 {
        let n = file.read(&mut buf[..(left as usize).min(BUF)])?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short"));
        }
        h.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(h.finalize())
}

/// The journal for this download: kept chunks are re-checked against their CRC; a ".part" file
/// from an older version (no journal) counts as done for the whole chunks it already holds.
fn open_book(part: &Path, total: u64, restart: bool) -> Result<Book, Fail> {
    let path = PathBuf::from(format!("{}.chunks", part.display()));
    if restart {
        let _ = std::fs::remove_file(part);
        let _ = std::fs::remove_file(&path);
    }
    let mut journal = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Journal>(&b).ok())
        .filter(|j| j.total == total && j.chunk == CHUNK)
        .unwrap_or_else(|| Journal { total, chunk: CHUNK, done: HashMap::new() });

    let had_journal = path.exists() && !journal.done.is_empty();
    if let Ok(mut f) = File::open(part) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if had_journal {
            journal.done.retain(|&i, crc| {
                let (s, e) = (i * CHUNK, ((i + 1) * CHUNK).min(total));
                e <= len && crc_of_region(&mut f, s, e).map(|c| c == *crc).unwrap_or(false)
            });
        } else if len > 0 && len <= total {
            for i in 0..(len / CHUNK) {
                let (s, e) = (i * CHUNK, (i + 1) * CHUNK);
                if let Ok(c) = crc_of_region(&mut f, s, e) {
                    journal.done.insert(i, c);
                }
            }
            if len == total && total % CHUNK != 0 {
                let i = total / CHUNK;
                if let Ok(c) = crc_of_region(&mut f, i * CHUNK, total) {
                    journal.done.insert(i, c);
                }
            }
        }
    } else {
        journal.done.clear();
    }
    Ok(Book { path, part: part.to_path_buf(), journal: Mutex::new(journal) })
}

// ── Requests ──────────────────────────────────────────────────────────────────

fn headers(job: &Job) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in &job.headers {
        if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            h.insert(name, value);
        }
    }
    h
}

fn content_range_start(resp: &Response) -> Option<u64> {
    let v = resp.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    v.trim().strip_prefix("bytes")?.trim().split('-').next()?.trim().parse().ok()
}

/// GET with an optional byte range. A 200 answer to a ranged request is only usable from byte 0.
fn get(job: &Job, range: Option<(u64, Option<u64>)>) -> Result<(Response, u64), Fail> {
    let mut req = client().get(&job.url).headers(headers(job));
    if let Some((a, b)) = range {
        let v = match b {
            Some(b) => format!("bytes={a}-{b}"),
            None => format!("bytes={a}-"),
        };
        req = req.header(RANGE, v);
    }
    let resp = req.send().map_err(|e| Fail::Network(format!("Connection error: {e}")))?;
    let code = resp.status().as_u16();
    match code {
        206 => {
            let start = content_range_start(&resp).unwrap_or(range.map(|r| r.0).unwrap_or(0));
            if range.is_some_and(|r| r.0 != start) {
                return Err(Fail::Other("The server sent a different part of the file than asked".into()));
            }
            Ok((resp, start))
        }
        200 => Ok((resp, 0)),
        429 => Err(Fail::Http(429, "429_RATE_LIMIT".into())),
        403 => Err(Fail::Http(403, "403 Forbidden".into())),
        404 => Err(Fail::Http(404, "404 Not Found — file does not exist on server".into())),
        416 => Err(Fail::Http(416, "The server refused the byte range".into())),
        c => Err(Fail::Http(c, format!("HTTP {c}"))),
    }
}

fn retryable(f: &Fail) -> bool {
    match f {
        Fail::Network(_) => true,
        Fail::Http(c, _) => *c >= 500 || *c == 429,
        _ => false,
    }
}

fn backoff(attempt: u32, p: &Progress) -> Result<(), Fail> {
    let wait = Duration::from_millis(1000 * (1 << attempt.min(4)));
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < wait {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Fail::Cancelled);
        }
        std::thread::sleep(step);
        slept += step;
    }
    Ok(())
}

/// Streams `resp` into `file` from `pos` until `end` (exclusive) or the body ends, marking chunks
/// done as their last byte lands. Returns the position reached.
fn pump(resp: &mut Response, file: &mut File, mut pos: u64, end: u64, book: &Book, p: &Progress) -> Result<u64, Fail> {
    file.seek(SeekFrom::Start(pos)).map_err(io_fail)?;
    let mut crc = crc32fast::Hasher::new();
    let mut crc_from = pos; // the chunk CRC only counts when we wrote the chunk from its start
    let mut buf = vec![0u8; BUF];
    while pos < end {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Fail::Cancelled);
        }
        let want = ((end - pos) as usize).min(BUF);
        let n = match resp.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                file.flush().map_err(io_fail)?;
                return Err(Fail::Network(format!("Connection dropped mid-download: {e}")));
            }
        };
        limiter::take(n as u64, &p.cancel);
        file.write_all(&buf[..n]).map_err(io_fail)?;
        let mut slice = &buf[..n];
        // Split the write at chunk boundaries so each chunk gets its own CRC
        while !slice.is_empty() {
            let chunk_end = ((pos / CHUNK) + 1) * CHUNK;
            let take = ((chunk_end - pos) as usize).min(slice.len());
            crc.update(&slice[..take]);
            pos += take as u64;
            p.done.fetch_add(take as u64, Ordering::Relaxed);
            slice = &slice[take..];
            let total = book.journal.lock().unwrap().total;
            if pos == chunk_end || (total > 0 && pos == total) {
                let i = (pos - 1) / CHUNK;
                if crc_from == i * CHUNK {
                    file.flush().map_err(io_fail)?;
                    book.finish(i, std::mem::take(&mut crc).finalize());
                } else {
                    crc = crc32fast::Hasher::new();
                }
                crc_from = pos;
            }
        }
    }
    file.flush().map_err(io_fail)?;
    Ok(pos)
}

fn open_part(part: &Path) -> Result<File, Fail> {
    if let Some(dir) = part.parent() {
        std::fs::create_dir_all(dir).map_err(io_fail)?;
    }
    OpenOptions::new().create(true).truncate(false).write(true).read(true).open(part).map_err(io_fail)
}

/// Fetches [start, end) with retries, continuing from the last byte written after a drop.
fn fetch_span(job: &Job, file: &mut File, start: u64, end: u64, book: &Book, p: &Progress) -> Result<(), Fail> {
    let mut pos = start;
    let mut attempt = 0;
    while pos < end {
        let result = get(job, Some((pos, Some(end - 1)))).and_then(|(mut resp, got)| {
            if got != pos {
                return Err(Fail::Other("The server ignored the byte range".into()));
            }
            pump(&mut resp, file, pos, end, book, p)
        });
        match result {
            Ok(reached) => {
                if reached > pos {
                    attempt = 0;
                }
                if reached < end {
                    // the body ended early: count it like a drop
                    attempt += 1;
                    if attempt >= MAX_ATTEMPTS {
                        return Err(Fail::Network("The connection kept ending early".into()));
                    }
                    backoff(attempt, p)?;
                }
                pos = reached;
            }
            Err(f) if retryable(&f) && attempt + 1 < MAX_ATTEMPTS => {
                attempt += 1;
                // bytes written before the drop stay counted; continue right after them
                pos = file.stream_position().unwrap_or(pos).max(pos);
                backoff(attempt, p)?;
            }
            Err(f) => return Err(f),
        }
    }
    // A chunk resumed part-way through (after a drop) wasn't checksummed while streaming: record
    // it now from what's on disk
    file.flush().map_err(io_fail)?;
    let mut reader = File::open(&book.part).map_err(io_fail)?;
    for i in (start / CHUNK)..end.div_ceil(CHUNK) {
        let (a, b) = book.span(i);
        if a >= start && b <= end && !book.is_done(i) {
            let crc = crc_of_region(&mut reader, a, b).map_err(io_fail)?;
            book.finish(i, crc);
        }
    }
    Ok(())
}

// ── The download ────────────────────────────────────────────────────────────

pub fn run(job: &Job, p: &Arc<Progress>) -> Result<Done, Fail> {
    limiter::set(job.limit_bps);
    let part = PathBuf::from(&job.part_path);
    if job.total == 0 {
        return run_unknown_size(job, &part, p);
    }
    let total = job.total;
    p.total.store(total, Ordering::Relaxed);
    let book = Arc::new(open_book(&part, total, job.restart)?);
    let resumed_from = book.done_bytes();
    p.done.store(resumed_from, Ordering::Relaxed);
    let missing: Vec<u64> = (0..Book::chunks(total)).filter(|i| !book.is_done(*i)).collect();

    if !missing.is_empty() {
        let mut file = open_part(&part)?;
        if job.connections <= 1 || !job.ranges {
            // One connection: stream each missing stretch in order
            let mut i = 0;
            while i < missing.len() {
                let first = missing[i];
                let mut last = first;
                while i + 1 < missing.len() && missing[i + 1] == last + 1 {
                    i += 1;
                    last = missing[i];
                }
                i += 1;
                let (start, _) = book.span(first);
                let (_, end) = book.span(last);
                if !job.ranges {
                    if start != 0 {
                        // No ranges: everything again from the start
                        drop(file);
                        return run(&Job { restart: true, ..clone_job(job) }, p);
                    }
                    let (mut resp, _) = get(job, None)?;
                    let reached = pump(&mut resp, &mut file, 0, total, &book, p)?;
                    if reached < total {
                        return Err(Fail::Network("The connection ended before the whole file arrived".into()));
                    }
                } else {
                    fetch_span(job, &mut file, start, end, &book, p)?;
                }
            }
        } else {
            // Several connections, each taking the next missing chunk
            let queue = Arc::new(Mutex::new(missing.into_iter()));
            let failed: Arc<Mutex<Option<Fail>>> = Arc::default();
            let active = Arc::new(AtomicUsize::new(0));
            std::thread::scope(|s| {
                for _ in 0..job.connections.clamp(1, 8) {
                    let (queue, failed, book, active, part) = (queue.clone(), failed.clone(), book.clone(), active.clone(), part.clone());
                    s.spawn(move || {
                        active.fetch_add(1, Ordering::Relaxed);
                        let mut file = match open_part(&part) {
                            Ok(f) => f,
                            Err(e) => {
                                failed.lock().unwrap().get_or_insert(e);
                                return;
                            }
                        };
                        loop {
                            if failed.lock().unwrap().is_some() {
                                break;
                            }
                            let Some(i) = queue.lock().unwrap().next() else { break };
                            let (a, b) = book.span(i);
                            if let Err(e) = fetch_span(job, &mut file, a, b, &book, p) {
                                p.cancel.store(true, Ordering::Relaxed); // stop the others too
                                failed.lock().unwrap().get_or_insert(e);
                                break;
                            }
                        }
                    });
                }
            });
            if let Some(f) = failed.lock().unwrap().take() {
                // a sibling's own cancel isn't the reason
                return Err(f);
            }
        }
        file_set_len(&part, total)?;
    }

    if (0..Book::chunks(total)).any(|i| !book.is_done(i)) {
        return Err(Fail::Network("Some parts of the file are still missing".into()));
    }
    let sha256 = if job.hash { sha_of_file(&part, p)? } else { String::new() };
    let _ = std::fs::remove_file(&book.path);
    Ok(Done { size: total, sha256, resumed_from })
}

fn clone_job(j: &Job) -> Job {
    Job {
        url: j.url.clone(),
        part_path: j.part_path.clone(),
        headers: j.headers.clone(),
        total: j.total,
        ranges: j.ranges,
        connections: j.connections,
        restart: j.restart,
        hash: j.hash,
        limit_bps: j.limit_bps,
    }
}

fn file_set_len(part: &Path, total: u64) -> Result<(), Fail> {
    let f = OpenOptions::new().write(true).open(part).map_err(io_fail)?;
    if f.metadata().map_err(io_fail)?.len() != total {
        f.set_len(total).map_err(io_fail)?;
    }
    Ok(())
}

fn sha_of_file(part: &Path, p: &Progress) -> Result<String, Fail> {
    let mut f = File::open(part).map_err(io_fail)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Fail::Cancelled);
        }
        let n = f.read(&mut buf).map_err(io_fail)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A file whose size the server doesn't say: one stream from the start, hashed as it arrives.
fn run_unknown_size(job: &Job, part: &Path, p: &Arc<Progress>) -> Result<Done, Fail> {
    let _ = std::fs::remove_file(PathBuf::from(format!("{}.chunks", part.display())));
    let mut file = open_part(part)?;
    file.set_len(0).map_err(io_fail)?;
    let (mut resp, _) = get(job, None)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BUF];
    let mut size = 0u64;
    loop {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Fail::Cancelled);
        }
        let n = resp.read(&mut buf).map_err(|e| Fail::Network(format!("Connection dropped mid-download: {e}")))?;
        if n == 0 {
            break;
        }
        limiter::take(n as u64, &p.cancel);
        file.write_all(&buf[..n]).map_err(io_fail)?;
        if job.hash {
            h.update(&buf[..n]);
        }
        size += n as u64;
        p.done.store(size, Ordering::Relaxed);
    }
    file.flush().map_err(io_fail)?;
    let sha256 = if job.hash { h.finalize().iter().map(|b| format!("{b:02x}")).collect() } else { String::new() };
    Ok(Done { size, sha256, resumed_from: 0 })
}
