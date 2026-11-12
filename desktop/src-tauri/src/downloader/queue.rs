//! The download queue: its files, the worker pool that downloads them (adaptive thread count,
//! a cooldown when servers answer 429, at most two Telegram files at once), pausing / resuming /
//! cancelling everything, a batch or one file, retries (automatic at the end, Retry Failed,
//! selected files), and the overall speed, ETA and progress the window shows.
//! A port of FileDownloader's queue and run loop (core/downloader.py).

use super::fetch::{self, format_speed, Cell, Ctl, Hooks, Item, State};
use super::filters::Options;
use super::plan::{write_post_info, Task};
use super::py::{basename, dirname, splitext};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What the queue tells the rest of the app, on top of what a file download asks
pub trait QueueHooks: Hooks {
    /// Overall progress, about five times a second (see `Queue::progress_info`)
    fn progress(&self, _info: &Value) {}
    /// The run ended: (finished normally, summary)
    fn finished(&self, _ok: bool, _summary: &str) {}
    /// The number of files downloaded at once changed
    fn concurrency(&self, _workers: usize) {}
    fn pause_changed(&self, _paused: bool) {}
    /// Many files changed at once (retries)
    fn many_changed(&self, cells: &[Cell]) {
        for c in cells {
            self.changed(c);
        }
    }
    /// A file was downloaded (the session's record of downloaded files)
    fn file_completed(&self, _t: &Task) {}
    /// Save / drop the crash-recovery copy of the queue
    fn checkpoint(&self) {}
    fn discard_recovery(&self) {}
    /// Cancel downloads the app runs itself (yt-dlp, Telegram, cloud links)
    fn cancel_others(&self) {}
    /// Cloud links (MEGA, Drive…) in the queue are pages the cloud downloader handles
    fn is_cloud_link(&self, _url: &str) -> bool {
        false
    }
    fn log(&self, _level: &str, _message: &str) {}
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

const FATAL_AUTH: &[&str] = &["key is not registered", "session expired", "session revoked", "authkey", "auth_key", "unregistered", "unauthorized"];
const ACTIVE: &[&str] = &["pending", "downloading", "retrying"];

fn is_telegram(t: &Task) -> bool {
    t.is_telegram || t.service == "telegram" || t.url.starts_with("tg://")
}

fn is_404(it: &Item) -> bool {
    it.st.error_msg.to_lowercase().contains("404") || it.st.http_status == 404
}

/// The run's numbers for speed and ETA
#[derive(Default)]
struct Meter {
    start: f64,
    paused_total: f64,
    pause_start: f64,
    first_byte: f64,
    samples: VecDeque<(f64, u64)>,
    rolling: VecDeque<(f64, u64)>,
    smoothed: f64,
    medium: f64,
    smoothed_eta: Option<f64>,
    last_eta_calc: f64,
    last_emit: f64,
    stats: Option<(f64, Stats)>,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    done_bytes: f64,
    done_n: f64,
    known_bytes: f64,
    known_n: f64,
    known_remaining: f64,
    unknown_n: f64,
    unknown_downloaded: f64,
    all_downloaded: u64,
    done_video_bytes: f64,
    done_video_n: f64,
    done_image_bytes: f64,
    done_image_n: f64,
    unknown_video_n: f64,
    unknown_image_n: f64,
    unknown_other_n: f64,
}

struct Run {
    running: bool,
    session: u64,
    max_workers: usize,
    learned_ceiling: Option<usize>,
    cooldown_until: f64,
    stable_clean: usize,
    adaptive_state: String,
    adaptive_text: String,
    options: Option<Arc<Options>>,
    run_tasks: Vec<Cell>,
    cursor: usize,
    last_full_scan: f64,
    meter: Meter,
}

pub struct Queue {
    pub items: Mutex<Vec<Cell>>,
    pub ctl: Arc<Ctl>,
    hooks: Arc<dyn QueueHooks>,
    run: Mutex<Run>,
    batch_options: Mutex<HashMap<String, Arc<Options>>>,
    /// post_info.txt texts written once a file of their post is saved: (path, post id) → text
    post_infos: Mutex<HashMap<(String, String), String>>,
}

impl Queue {
    pub fn new(hooks: Arc<dyn QueueHooks>, max_workers: usize) -> Arc<Self> {
        Arc::new(Self {
            items: Mutex::default(),
            ctl: Arc::default(),
            hooks,
            run: Mutex::new(Run {
                running: false,
                session: 0,
                max_workers: max_workers.max(1),
                learned_ceiling: None,
                cooldown_until: 0.0,
                stable_clean: 0,
                adaptive_state: "optimal".into(),
                adaptive_text: String::new(),
                options: None,
                run_tasks: Vec::new(),
                cursor: 0,
                last_full_scan: 0.0,
                meter: Meter::default(),
            }),
            batch_options: Mutex::default(),
            post_infos: Mutex::default(),
        })
    }

    pub fn is_running(&self) -> bool {
        self.run.lock().unwrap().running
    }
    pub fn is_paused(&self) -> bool {
        self.ctl.paused()
    }
    pub fn max_workers(&self) -> usize {
        self.run.lock().unwrap().max_workers
    }
    pub fn set_max_workers(&self, n: usize) {
        self.run.lock().unwrap().max_workers = n.max(1);
    }
    pub fn cells(&self) -> Vec<Cell> {
        self.items.lock().unwrap().clone()
    }
    /// Posts' info texts from a plan, written once one of their files is saved
    pub fn add_post_infos(&self, infos: &[(String, String, String)]) {
        let mut m = self.post_infos.lock().unwrap();
        for (path, id, text) in infos {
            m.insert((path.clone(), id.clone()), text.clone());
        }
    }

    fn write_pending_post_info(&self, t: &Task) {
        if t.post_info_path.is_empty() {
            return;
        }
        let text = self.post_infos.lock().unwrap().remove(&(t.post_info_path.clone(), t.post_id.clone()));
        if let Some(text) = text {
            write_post_info(&t.post_info_path, &text, &t.post_id);
        }
    }

    pub fn set_batch_options(&self, batch: &str, o: Arc<Options>) {
        if !batch.is_empty() {
            self.batch_options.lock().unwrap().insert(batch.into(), o);
        }
    }

    fn options_for(&self, it: &Item, run: &Arc<Options>) -> Arc<Options> {
        it.options.clone().or_else(|| self.batch_options.lock().unwrap().get(&it.task.batch_id).cloned()).unwrap_or_else(|| run.clone())
    }

    /// Two different files may never share a save path: files not downloaded yet get
    /// "name (2).ext", "name (3).ext"… Returns how many were renamed.
    fn keep_paths_apart(new: &[Cell], existing: &[Cell]) -> usize {
        let norm = |p: &str| p.replace('/', "\\").to_lowercase();
        let mut taken: HashMap<String, (String, usize)> = HashMap::new(); // path -> (url, which cell)
        for c in existing {
            let it = c.lock().unwrap();
            if !it.task.target_path.is_empty() {
                taken.insert(norm(&it.task.target_path), (it.task.url.clone(), Arc::as_ptr(c) as usize));
            }
        }
        let mut renamed = 0;
        for c in new {
            let mut it = c.lock().unwrap();
            if it.task.target_path.is_empty() {
                continue;
            }
            let me = Arc::as_ptr(c) as usize;
            let mut key = norm(&it.task.target_path);
            if let Some((url, who)) = taken.get(&key) {
                if *who != me && *url != it.task.url && ["pending", "failed", "cancelled"].contains(&it.st.status.as_str()) {
                    let (stem, ext) = splitext(&it.task.target_path);
                    let (stem, ext) = (stem.to_string(), ext.to_string());
                    let mut n = 2;
                    while taken.contains_key(&norm(&format!("{stem} ({n}){ext}"))) {
                        n += 1;
                    }
                    it.task.target_path = format!("{stem} ({n}){ext}");
                    key = norm(&it.task.target_path);
                    renamed += 1;
                }
            }
            taken.entry(key).or_insert((it.task.url.clone(), me));
        }
        renamed
    }

    /// Adds files to the queue (without ones already in it) and starts it when idle and given
    /// settings. Returns how many were added.
    pub fn append(self: &Arc<Self>, tasks: Vec<Task>, options: Option<Arc<Options>>) -> usize {
        let items = tasks.into_iter().map(|t| (t, None, None)).collect();
        let n = self.add(items, options.clone()).len();
        if n > 0 && !self.is_running() {
            if let Some(o) = options {
                self.start(o);
            }
        }
        n
    }

    /// Adds files to the queue, nothing started: those already in it (same link and save path, or
    /// same file id) are left out, the others' save paths kept apart from every other file's. Each
    /// may come with its own state and settings; `options` are the settings of the ones without.
    /// Returns what was added: (key, save path).
    pub fn add(&self, tasks: Vec<(Task, Option<State>, Option<Arc<Options>>)>, options: Option<Arc<Options>>) -> Vec<(String, String)> {
        if tasks.is_empty() {
            return Vec::new();
        }
        let new: Vec<Cell> = {
            let items = self.items.lock().unwrap();
            let mut sigs: HashSet<String> = HashSet::new();
            for c in items.iter() {
                let it = c.lock().unwrap();
                sigs.insert(format!("{}\u{1}{}", it.task.url, it.task.target_path));
                if !it.task.file_id.is_empty() {
                    sigs.insert(it.task.file_id.clone());
                }
            }
            let mut out = Vec::new();
            for (t, st, o) in tasks {
                let s1 = format!("{}\u{1}{}", t.url, t.target_path);
                if sigs.contains(&s1) || (!t.file_id.is_empty() && sigs.contains(&t.file_id)) {
                    continue;
                }
                sigs.insert(s1);
                if !t.file_id.is_empty() {
                    sigs.insert(t.file_id.clone());
                }
                let c = fetch::cell(t);
                {
                    let mut it = c.lock().unwrap();
                    if let Some(st) = st {
                        it.st = st;
                    }
                    if o.is_some() {
                        it.options = o;
                    }
                }
                out.push(c);
            }
            out
        };
        if new.is_empty() {
            self.hooks.log("info", "All new tasks were duplicates and already exist in the queue.");
            return Vec::new();
        }
        Self::keep_paths_apart(&new, &self.cells());
        if let Some(o) = &options {
            for c in &new {
                let mut it = c.lock().unwrap();
                if it.options.is_none() {
                    it.options = Some(o.clone());
                }
                let b = it.task.batch_id.clone();
                drop(it);
                self.set_batch_options(&b, o.clone());
            }
        }
        let added: Vec<(String, String)> = new.iter().map(|c| {
            let it = c.lock().unwrap();
            (it.task.key.clone(), it.task.target_path.clone())
        }).collect();
        let n = new.len();
        self.items.lock().unwrap().extend(new);
        self.hooks.log("info", &format!("Appended {n} new tasks to download queue (total in queue: {}).", self.items.lock().unwrap().len()));
        self.rescan();
        added
    }

    /// Replaces the whole queue (a restored session)
    pub fn set_items(&self, cells: Vec<Cell>) {
        *self.items.lock().unwrap() = cells;
    }

    /// The app's queue as it is now, by key: files already here keep their download state (the
    /// app reports status changes separately), new ones come with theirs, missing ones leave
    /// (unless they're being downloaded)
    /// With `replace_state` (before a run starts) the app's statuses win for every file.
    pub fn sync(&self, incoming: Vec<(Task, fetch::State, Option<Arc<Options>>)>, replace_state: bool) {
        let mut items = self.items.lock().unwrap();
        let mut by_key: HashMap<String, Cell> = items.drain(..).map(|c| {
            let k = c.lock().unwrap().task.key.clone();
            (k, c)
        }).collect();
        let mut out = Vec::with_capacity(incoming.len());
        for (task, st, o) in incoming {
            match by_key.remove(&task.key) {
                Some(c) => {
                    {
                        let mut it = c.lock().unwrap();
                        if o.is_some() {
                            it.options = o;
                        }
                        if it.st.status != "downloading" {
                            it.task = task;
                            if replace_state {
                                it.st = st;
                            }
                        }
                    }
                    out.push(c);
                }
                None => out.push(Arc::new(Mutex::new(Item { task, st, options: o }))),
            }
        }
        out.extend(by_key.into_values().filter(|c| c.lock().unwrap().st.status == "downloading"));
        *items = out;
    }

    pub fn find_key(&self, key: &str) -> Option<Cell> {
        self.cells().into_iter().find(|c| c.lock().unwrap().task.key == key)
    }

    /// The app changed a file's status (paused, cancelled, put back in line…)
    pub fn set_state(&self, key: &str, status: &str, error_msg: Option<&str>, retry_count: Option<u32>) {
        let Some(c) = self.find_key(key) else { return };
        let mut it = c.lock().unwrap();
        it.st.status = status.into();
        if let Some(e) = error_msg {
            it.st.error_msg = e.into();
        }
        if let Some(n) = retry_count {
            it.st.retry_count = n;
        }
        if status == "pending" {
            drop(it);
            self.rescan();
        }
    }

    pub fn set_batch_paused(&self, batch: &str, paused: bool) {
        let mut b = self.ctl.paused_batches.lock().unwrap();
        if paused {
            b.insert(batch.into());
        } else {
            b.remove(batch);
            drop(b);
            self.rescan();
        }
    }

    /// Starts downloading the queue's pending files
    pub fn start(self: &Arc<Self>, options: Arc<Options>) {
        let session = {
            let mut r = self.run.lock().unwrap();
            r.session += 1;
            r.cursor = 0;
            r.options = Some(options.clone());
            r.running = true;
            r.session
        };
        // Files a previous run was downloading go back in line
        for c in self.cells() {
            let mut it = c.lock().unwrap();
            if it.st.status == "downloading" {
                it.st.status = "pending".into();
            }
        }
        self.ctl.cancel.store(false, Ordering::Relaxed);
        self.ctl.pause.store(false, Ordering::Relaxed);
        self.hooks.pause_changed(false);
        let me = self.clone();
        std::thread::spawn(move || me.run_loop(options, session));
    }

    // ── Controls ──────────────────────────────────────────────────────────────

    pub fn cancel(&self) {
        self.run.lock().unwrap().session += 1;
        self.ctl.cancel.store(true, Ordering::Relaxed);
        self.ctl.pause.store(false, Ordering::Relaxed);
        self.hooks.log("warning", "Download cancellation requested.");
        self.hooks.cancel_others();
        self.hooks.discard_recovery();
        self.hooks.pause_changed(false);
    }

    pub fn pause(&self) {
        self.ctl.pause.store(true, Ordering::Relaxed);
        let mut r = self.run.lock().unwrap();
        if r.meter.pause_start <= 0.0 {
            r.meter.pause_start = now();
        }
        drop(r);
        self.hooks.log("info", "Download paused.");
        self.hooks.checkpoint();
        self.hooks.pause_changed(true);
    }

    pub fn resume(&self) {
        let t = now();
        {
            let mut r = self.run.lock().unwrap();
            let m = &mut r.meter;
            if m.pause_start > 0.0 {
                m.paused_total += (t - m.pause_start).max(0.0);
                m.pause_start = 0.0;
            }
            m.samples.clear();
            m.rolling.clear();
            m.smoothed = 0.0;
            m.last_eta_calc = t;
        }
        self.ctl.pause.store(false, Ordering::Relaxed);
        self.hooks.log("info", "Download resumed.");
        self.hooks.pause_changed(false);
    }

    /// Empties the queue and its state for a new session
    pub fn reset(&self) {
        {
            let mut r = self.run.lock().unwrap();
            r.session += 1;
            r.running = false;
            r.meter = Meter::default();
            r.adaptive_state = "optimal".into();
            r.adaptive_text.clear();
        }
        self.ctl.cancel.store(false, Ordering::Relaxed);
        self.ctl.pause.store(false, Ordering::Relaxed);
        self.ctl.paused_batches.lock().unwrap().clear();
        self.ctl.bytes.store(0, Ordering::Relaxed);
        self.items.lock().unwrap().clear();
        self.hooks.pause_changed(false);
    }

    fn batch_cells(&self, batch: &str) -> Vec<Cell> {
        self.cells().into_iter().filter(|c| c.lock().unwrap().task.batch_id == batch).collect()
    }

    pub fn pause_batch(&self, batch: &str) -> usize {
        if batch.is_empty() {
            return 0;
        }
        self.ctl.paused_batches.lock().unwrap().insert(batch.into());
        let mut n = 0;
        for c in self.batch_cells(batch) {
            let mut it = c.lock().unwrap();
            if ACTIVE.contains(&it.st.status.as_str()) {
                it.st.status = "paused".into();
                it.st.stop_moving();
                it.st.eta_str = "--".into();
                n += 1;
                drop(it);
                self.hooks.changed(&c);
            }
        }
        n
    }

    fn find(&self, file_id: &str) -> Option<Cell> {
        self.cells().into_iter().find(|c| c.lock().unwrap().task.file_id == file_id)
    }

    pub fn pause_task(&self, file_id: &str) -> bool {
        let Some(c) = self.find(file_id) else { return false };
        let mut it = c.lock().unwrap();
        if !["pending", "downloading", "retrying", "paused"].contains(&it.st.status.as_str()) {
            return false;
        }
        it.st.status = "paused".into();
        it.st.stop_moving();
        it.st.eta_str = "--".into();
        drop(it);
        self.hooks.changed(&c);
        true
    }

    fn rescan(&self) {
        let mut r = self.run.lock().unwrap();
        r.cursor = 0;
        r.last_full_scan = 0.0;
    }

    fn run_options(&self, given: Option<Arc<Options>>) -> Arc<Options> {
        given.or_else(|| self.run.lock().unwrap().options.clone()).unwrap_or_else(|| Arc::new(Options::from_dict(&json!({}))))
    }

    pub fn resume_task(self: &Arc<Self>, file_id: &str, options: Option<Arc<Options>>) -> bool {
        if !self.unpause_task(file_id) {
            return false;
        }
        if !self.is_running() {
            self.start(self.run_options(options));
        }
        true
    }

    /// A paused file back in line (nothing is started)
    pub fn unpause_task(&self, file_id: &str) -> bool {
        let Some(c) = self.find(file_id) else { return false };
        {
            let mut it = c.lock().unwrap();
            if !["paused", "pending"].contains(&it.st.status.as_str()) {
                return false;
            }
            it.st.status = "pending".into();
            it.st.error_msg.clear();
        }
        self.hooks.changed(&c);
        self.rescan();
        true
    }

    pub fn resume_batch(self: &Arc<Self>, batch: &str, options: Option<Arc<Options>>) -> usize {
        let n = self.unpause_batch(batch);
        if n > 0 && !self.is_running() {
            self.start(self.run_options(options));
        }
        n
    }

    /// A paused post's files back in line (nothing is started): how many
    pub fn unpause_batch(&self, batch: &str) -> usize {
        if batch.is_empty() {
            return 0;
        }
        self.ctl.paused_batches.lock().unwrap().remove(batch);
        self.rescan();
        let mut n = 0;
        for c in self.batch_cells(batch) {
            let mut it = c.lock().unwrap();
            if it.st.status == "paused" {
                it.st.status = "pending".into();
                it.st.error_msg.clear();
                n += 1;
                drop(it);
                self.hooks.changed(&c);
            }
        }
        n
    }

    /// Whether the queue has this post's files / this file
    pub fn has_batch(&self, batch: &str) -> bool {
        !batch.is_empty() && self.cells().iter().any(|c| c.lock().unwrap().task.batch_id == batch)
    }
    pub fn has_file(&self, file_id: &str) -> bool {
        !file_id.is_empty() && self.find(file_id).is_some()
    }
    pub fn is_batch_paused(&self, batch: &str) -> bool {
        self.ctl.paused_batches.lock().unwrap().contains(batch)
    }
    /// The file's name (for the log)
    pub fn file_name(&self, file_id: &str) -> String {
        self.find(file_id).map(|c| basename(&c.lock().unwrap().task.target_path).to_string()).unwrap_or_default()
    }

    /// Whether files are waiting to be downloaded (of one post, or any)
    pub fn has_waiting(&self, batch: Option<&str>) -> bool {
        self.cells().iter().any(|c| {
            let it = c.lock().unwrap();
            it.st.status == "pending" && batch.is_none_or(|b| it.task.batch_id == b)
        })
    }

    pub fn cancel_batch(&self, batch: &str, notify: bool) -> usize {
        self.ctl.paused_batches.lock().unwrap().remove(batch);
        let mut n = 0;
        for c in self.batch_cells(batch) {
            let mut it = c.lock().unwrap();
            if ["pending", "downloading", "retrying", "paused"].contains(&it.st.status.as_str()) {
                it.st.status = "cancelled".into();
                it.st.error_msg = "Download cancelled by user".into();
                it.st.progress_pct = 0;
                it.st.stop_moving();
                it.st.eta_str = "--".into();
                n += 1;
                drop(it);
                if notify {
                    self.hooks.changed(&c);
                }
            }
        }
        n
    }

    fn reset_for_retry(it: &mut Item) {
        it.st.retry_count += 1;
        it.st.retry_capped = false;
        it.st.status = "pending".into();
        it.st.error_msg.clear();
        it.st.http_status = 0;
        it.st.progress_pct = 0;
        it.st.downloaded_bytes = 0;
        it.st.stop_moving();
        it.st.eta_str = "--".into();
    }

    pub fn retry_batch_failed(self: &Arc<Self>, batch: &str, options: Option<Arc<Options>>) -> usize {
        self.ctl.paused_batches.lock().unwrap().remove(batch);
        let running = self.is_running();
        let mut list = Vec::new();
        for c in self.batch_cells(batch) {
            let mut it = c.lock().unwrap();
            let s = it.st.status.as_str();
            if ["failed", "cancelled"].contains(&s) || (!running && ["pending", "paused"].contains(&s)) {
                it.st.status = "pending".into();
                it.st.error_msg.clear();
                it.st.downloaded_bytes = 0;
                it.st.progress_pct = 0;
                it.st.retry_count += 1;
                drop(it);
                self.hooks.changed(&c);
                list.push(c);
            }
        }
        if !list.is_empty() && !running {
            if let Some(o) = options {
                self.start(o);
            }
        }
        list.len()
    }

    /// Clear failed: the failed and cancelled files leave the queue (with `ids`, only those whose
    /// file id, link or file name is one of them). Returns their keys.
    pub fn clear_failed(&self, ids: Option<&HashSet<String>>) -> Vec<String> {
        let mut gone = Vec::new();
        self.items.lock().unwrap().retain(|c| {
            let it = c.lock().unwrap();
            let t = &it.task;
            let hit = matches!(it.st.status.as_str(), "failed" | "cancelled")
                && ids.is_none_or(|s| s.contains(&t.file_id) || s.contains(&t.url) || s.contains(basename(&t.target_path)));
            if hit {
                gone.push(t.key.clone());
            }
            !hit
        });
        gone
    }

    /// Removes a batch's files that aren't being downloaded right now
    pub fn remove_batch(&self, batch: &str) -> usize {
        self.ctl.paused_batches.lock().unwrap().remove(batch);
        let mut items = self.items.lock().unwrap();
        let before = items.len();
        items.retain(|c| {
            let it = c.lock().unwrap();
            it.task.batch_id != batch || it.st.status == "downloading"
        });
        before - items.len()
    }

    fn skip_404(&self, options: Option<&Arc<Options>>) -> bool {
        options.cloned().or_else(|| self.run.lock().unwrap().options.clone()).is_some_and(|o| o.flag("skip_retry_404"))
    }

    /// Retry Failed: failed (and cancelled, unless `include_cancelled` is off) files go back in
    /// line, at most `max_retries` times each. Returns how many.
    pub fn retry_failed(self: &Arc<Self>, options: Arc<Options>, max_retries: u32, include_cancelled: bool) -> usize {
        let skip_404 = self.skip_404(Some(&options));
        let running = self.is_running();
        let (n, any) = self.retry_failed_cells(skip_404, running, max_retries, include_cancelled, false);
        if !any {
            self.hooks.log("info", "No failed tasks to retry.");
            return 0;
        }
        if n > 0 && !running {
            self.start(options);
        }
        n
    }

    /// Retry Failed's work, nothing started: (how many went back in line, whether any file could be
    /// retried at all). With `skip_cloud`, cloud links are left alone (the app retries those).
    pub fn retry_failed_cells(&self, skip_404: bool, running: bool, max_retries: u32, include_cancelled: bool, skip_cloud: bool) -> (usize, bool) {
        let statuses: &[&str] = if include_cancelled { &["failed", "cancelled"] } else { &["failed"] };
        let (mut eligible, mut capped) = (Vec::new(), Vec::new());
        let mut any = false;
        for c in self.cells() {
            let mut it = c.lock().unwrap();
            if skip_cloud && self.hooks.is_cloud_link(&it.task.url) {
                continue;
            }
            let s = it.st.status.clone();
            let candidate = statuses.contains(&s.as_str()) || (!running && s == "pending" && (!it.st.error_msg.is_empty() || it.st.retry_count > 0));
            if !candidate {
                continue;
            }
            any = true;
            let err = it.st.error_msg.to_lowercase();
            if skip_404 && is_404(&it) {
                continue;
            }
            if FATAL_AUTH.iter().any(|f| err.contains(f)) {
                it.st.retry_capped = true;
                continue;
            }
            if it.st.retry_count >= max_retries {
                it.st.retry_capped = true;
                let orig = if it.st.error_msg.is_empty() { "Download failed".to_string() } else { it.st.error_msg.clone() };
                if !orig.contains(&format!("({max_retries} retries)")) {
                    it.st.error_msg = format!("{orig} (Stopped after {max_retries} retries)");
                }
                drop(it);
                capped.push(c);
            } else {
                drop(it);
                eligible.push(c);
            }
        }
        self.hooks.many_changed(&capped);
        if !any {
            return (0, false);
        }
        for c in &eligible {
            Self::reset_for_retry(&mut c.lock().unwrap());
        }
        self.hooks.many_changed(&eligible);
        (eligible.len(), true)
    }

    /// The window's retry of some files (by their Retry-list names) or of a post's failed files,
    /// nothing started: failed / cancelled files, and waiting or paused ones when nothing runs.
    /// 404s are left out with `skip_404`, cloud links always (the app retries those). How many.
    pub fn retry_quiet(&self, keys: Option<&[String]>, batch: Option<&str>, skip_404: bool) -> usize {
        if let Some(b) = batch {
            self.ctl.paused_batches.lock().unwrap().remove(b);
        }
        let running = self.is_running();
        let wanted: Option<HashSet<&String>> = keys.map(|k| k.iter().collect());
        let mut list = Vec::new();
        for c in self.cells() {
            let mut it = c.lock().unwrap();
            if batch.is_some_and(|b| it.task.batch_id != b) || wanted.as_ref().is_some_and(|w| !w.contains(&Self::task_key(&it.task))) {
                continue;
            }
            let s = it.st.status.as_str();
            let ok = ["failed", "cancelled"].contains(&s) || (!running && ["pending", "paused"].contains(&s));
            if !ok || (skip_404 && is_404(&it)) || self.hooks.is_cloud_link(&it.task.url) {
                continue;
            }
            Self::reset_for_retry(&mut it);
            drop(it);
            list.push(c);
        }
        self.hooks.many_changed(&list);
        if !list.is_empty() {
            self.rescan();
        }
        list.len()
    }

    /// Whether the file (by its Retry-list name) is a cloud link (the app retries those)
    pub fn is_cloud_file(&self, key: &str) -> bool {
        self.cells().iter().any(|c| {
            let it = c.lock().unwrap();
            Self::task_key(&it.task) == key && self.hooks.is_cloud_link(&it.task.url)
        })
    }
    pub fn has_key(&self, key: &str) -> bool {
        self.cells().iter().any(|c| Self::task_key(&c.lock().unwrap().task) == key)
    }

    /// How the Retry Failed list names a file (its id, else its URL, else its name)
    pub fn task_key(t: &Task) -> String {
        if !t.file_id.is_empty() {
            t.file_id.clone()
        } else if !t.url.is_empty() {
            t.url.clone()
        } else {
            basename(&t.target_path).to_string()
        }
    }

    pub fn retry_selected(self: &Arc<Self>, keys: &[String], options: Arc<Options>) -> usize {
        if keys.is_empty() {
            return 0;
        }
        let skip_404 = self.skip_404(Some(&options));
        let running = self.is_running();
        let wanted: HashSet<&String> = keys.iter().collect();
        let mut list = Vec::new();
        for c in self.cells() {
            let mut it = c.lock().unwrap();
            if skip_404 && it.st.status == "pending" && is_404(&it) {
                it.st.status = "failed".into();
            }
            if !wanted.contains(&Self::task_key(&it.task)) {
                continue;
            }
            let ok = ["failed", "cancelled"].contains(&it.st.status.as_str()) || (!running && it.st.status == "pending");
            if !ok || (skip_404 && is_404(&it)) {
                continue;
            }
            Self::reset_for_retry(&mut it);
            drop(it);
            list.push(c);
        }
        self.hooks.many_changed(&list);
        if !list.is_empty() && !running {
            self.start(options);
        }
        list.len()
    }

    pub fn status_counts(&self) -> HashMap<String, usize> {
        let mut m: HashMap<String, usize> = HashMap::new();
        for c in self.cells() {
            *m.entry(c.lock().unwrap().st.status.clone()).or_insert(0) += 1;
        }
        m
    }

    // ── A 429 from a server: everything waits, fewer files at once ─────────────

    fn rate_limit_backoff(&self, locked: bool) {
        let mut r = self.run.lock().unwrap();
        let t = now();
        if t < r.cooldown_until {
            return;
        }
        if locked {
            r.cooldown_until = t + 30.0;
            let w = r.max_workers;
            drop(r);
            self.hooks.log("warning", &format!("⚡ [Rate Limit Cooldown] HTTP 429 encountered! Threads locked at {w} (cooldown 30s)..."));
            return;
        }
        r.cooldown_until = t + 14.0;
        let old = r.max_workers;
        let ceiling = match r.learned_ceiling {
            None => (old.saturating_sub(1)).max(1),
            Some(c) => c.min(old.saturating_sub(1)).max(1),
        };
        r.learned_ceiling = Some(ceiling);
        r.stable_clean = 0;
        r.max_workers = if ceiling > 2 { (old.saturating_sub(2)).min(ceiling - 1).max(1) } else { 1 };
        let w = r.max_workers;
        drop(r);
        self.hooks.log("warning", &format!("⚡ [Adaptive Backoff] HTTP 429 encountered! Locked stable ceiling to {ceiling} threads. Dropping concurrency to {w} threads (cooldown 14s)..."));
        self.hooks.concurrency(w);
    }

    // ── The run loop ──────────────────────────────────────────────────────────

    fn collect_pending(&self, limit: usize, t: f64, full: bool) -> Vec<Cell> {
        let items = self.cells();
        let mut r = self.run.lock().unwrap();
        if full || t - r.last_full_scan >= 1.0 || r.cursor > items.len() {
            r.cursor = 0;
            r.last_full_scan = t;
        }
        let mut found = Vec::new();
        let mut first = None;
        for (i, c) in items.iter().enumerate().skip(r.cursor) {
            let it = c.lock().unwrap();
            if it.st.status == "pending" && !self.ctl.batch_paused(&it.task.batch_id) {
                first.get_or_insert(i);
                found.push(c.clone());
                if found.len() >= limit {
                    break;
                }
            }
        }
        r.cursor = first.unwrap_or(items.len());
        found
    }

    fn current(&self, session: u64) -> bool {
        self.run.lock().unwrap().session == session
    }

    fn run_loop(self: Arc<Self>, options: Arc<Options>, session: u64) {
        let items = self.cells();
        Self::keep_paths_apart(&items, &[]);
        self.hooks.checkpoint();
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(16).max(4);
        let adaptive = options.flag("adaptive_threading");
        let mut locked = options.flag("threads_locked");
        let pure_telegram = !items.is_empty() && items.iter().all(|c| is_telegram(&c.lock().unwrap().task));
        let target_max;
        {
            let mut r = self.run.lock().unwrap();
            r.meter = Meter { start: now(), ..Default::default() };
            r.options = Some(options.clone());
            // Adaptive threading tops out at 8: beyond that servers rate-limit
            target_max = if adaptive && !locked { cores.min(8) } else { r.max_workers };
            if pure_telegram {
                r.max_workers = r.max_workers.min(2);
                locked = true;
            } else if adaptive && !locked {
                r.max_workers = 2.min(target_max);
            }
        }
        let adaptive = adaptive && !pure_telegram;
        let target_max = if pure_telegram { 2 } else { target_max };
        if pure_telegram || adaptive {
            self.hooks.concurrency(self.max_workers());
        }

        // The files this run downloads
        let skip_404 = options.flag("skip_retry_404");
        let mut run_tasks = Vec::new();
        for c in &items {
            let mut it = c.lock().unwrap();
            if skip_404 && it.st.status == "pending" && is_404(&it) {
                it.st.status = "failed".into();
            }
            if it.st.status == "pending" && !self.hooks.is_cloud_link(&it.task.url) {
                run_tasks.push(c.clone());
            }
        }
        self.run.lock().unwrap().run_tasks = run_tasks;
        self.ctl.already_there.lock().unwrap().clear();

        let (tx, rx): (Sender<(Cell, (bool, String))>, Receiver<_>) = channel();
        let mut active: Vec<Cell> = Vec::new();
        let (mut last_scale, mut last_checkpoint, mut last_count) = (now(), now(), 0.0);
        let mut successes = 0usize;
        let scale_interval = 8.0;

        while !self.ctl.cancelled() && self.current(session) {
            let t = now();
            let ceiling = self.run.lock().unwrap().learned_ceiling.unwrap_or(target_max);

            // 1. Scale up while things go well
            if adaptive && !locked && t - last_scale >= scale_interval {
                last_scale = t;
                let mut r = self.run.lock().unwrap();
                if t >= r.cooldown_until {
                    if r.max_workers < ceiling {
                        r.max_workers += 1;
                        successes = 0;
                        let w = r.max_workers;
                        drop(r);
                        self.hooks.concurrency(w);
                    } else if r.stable_clean >= 50 && ceiling < target_max {
                        // Long stability: probe one more
                        r.learned_ceiling = Some(ceiling + 1);
                        r.max_workers += 1;
                        r.stable_clean = 0;
                        successes = 0;
                        let w = r.max_workers;
                        drop(r);
                        self.hooks.concurrency(w);
                    }
                }
            }

            // 2. Cooling down after a 429
            let cooldown = self.run.lock().unwrap().cooldown_until;
            if t < cooldown {
                std::thread::sleep(Duration::from_secs_f64((cooldown - t).min(1.0)));
                continue;
            }
            while self.ctl.paused() && !self.ctl.cancelled() {
                std::thread::sleep(Duration::from_millis(300));
            }

            // 3. Finished files
            while let Ok((c, (ok, msg))) = rx.try_recv() {
                active.retain(|a| !Arc::ptr_eq(a, &c));
                self.file_done(&c, ok, &msg, &mut successes, locked);
                if ok && adaptive && !locked && successes >= 10 && t - last_scale >= 6.0 {
                    let mut r = self.run.lock().unwrap();
                    let ceiling = r.learned_ceiling.unwrap_or(target_max);
                    if t >= r.cooldown_until && r.max_workers < ceiling {
                        r.max_workers += 1;
                        successes = 0;
                        last_scale = t;
                        let w = r.max_workers;
                        drop(r);
                        self.hooks.concurrency(w);
                    }
                }
                if msg.starts_with("429_RATE_LIMIT") {
                    last_scale = now() + if locked { 30.0 } else { 15.0 };
                }
            }

            if t - last_count >= 0.3 {
                last_count = t;
                self.emit_progress(false);
            }

            // 4. Waiting (cooldown, paused, cancelled)
            if now() < self.run.lock().unwrap().cooldown_until || self.ctl.paused() || self.ctl.cancelled() {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }

            // 5. Start files up to the worker count, a little apart
            let slots = self.max_workers().saturating_sub(active.len());
            if slots > 0 {
                let mut pending = self.collect_pending(slots * 4 + 8, t, false);
                if pending.is_empty() && active.is_empty() {
                    pending = self.collect_pending(slots * 4 + 8, t, true);
                }
                if pending.is_empty() && active.is_empty() {
                    let paused_left = self.cells().iter().any(|c| {
                        let it = c.lock().unwrap();
                        ["pending", "paused"].contains(&it.st.status.as_str()) && (it.st.status == "paused" || self.ctl.batch_paused(&it.task.batch_id))
                    });
                    if paused_left && !self.ctl.cancelled() {
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    let opts = self.run.lock().unwrap().options.clone().unwrap_or(options.clone());
                    if opts.flag("auto_retry_at_end") && !self.ctl.cancelled() && self.auto_retry(&opts) {
                        continue;
                    }
                    break;
                }
                let mut tg_active = active.iter().filter(|c| is_telegram(&c.lock().unwrap().task)).count();
                let mut picked = Vec::new();
                for c in pending {
                    if picked.len() >= slots {
                        break;
                    }
                    if is_telegram(&c.lock().unwrap().task) {
                        if tg_active >= 2 {
                            continue;
                        }
                        tg_active += 1;
                    }
                    picked.push(c);
                }
                for (i, c) in picked.into_iter().enumerate() {
                    if self.ctl.cancelled() {
                        break;
                    }
                    if i > 0 {
                        let t = c.lock().unwrap().task.clone();
                        let pawchive = t.url.to_lowercase().contains("pawchive") || t.service.to_lowercase().contains("pawchive");
                        std::thread::sleep(Duration::from_secs_f64(if pawchive { super::sites::uniform(0.12, 0.22) } else { 0.06 }));
                        if self.ctl.cancelled() {
                            break;
                        }
                    }
                    let o = {
                        let mut it = c.lock().unwrap();
                        it.st.status = "downloading".into();
                        self.options_for(&it, &options)
                    };
                    self.hooks.changed(&c);
                    active.push(c.clone());
                    let (me, tx, c2) = (self.clone(), tx.clone(), c.clone());
                    std::thread::spawn(move || {
                        let hooks: &dyn Hooks = &*me.hooks;
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch::download(&c2, &o, hooks, &me.ctl)))
                            .unwrap_or_else(|_| (false, "Internal error while downloading".into()));
                        let _ = tx.send((c2, r));
                    });
                }
            }

            self.update_adaptive_state(adaptive, locked, target_max);
            if t - last_checkpoint >= 30.0 {
                last_checkpoint = t;
                self.hooks.checkpoint();
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // Cancelled: running files stop by themselves; wait briefly so their state settles
        let deadline = now() + 5.0;
        while !active.is_empty() && now() < deadline {
            if let Ok((c, _)) = rx.recv_timeout(Duration::from_millis(200)) {
                active.retain(|a| !Arc::ptr_eq(a, &c));
            }
        }
        if !self.current(session) && !self.ctl.cancelled() {
            return; // superseded by a newer run
        }
        self.finish_run();
    }

    /// One line in the log for a file's result, as the Python downloader wrote them
    fn log_result(&self, c: &Cell, ok: bool, msg: &str) {
        let (name, size) = {
            let it = c.lock().unwrap();
            (basename(&it.task.target_path).to_string(), it.task.file_size.max(it.st.downloaded_bytes as i64))
        };
        let mb = size as f64 / 1048576.0;
        if ok {
            if let Some(rest) = msg.strip_prefix("Success") {
                let check = rest.trim_start_matches(", ");
                let check = if check.is_empty() { String::new() } else { format!(", {check}") };
                self.hooks.log("success", &format!("✔ {name}  ({mb:.2} MB saved{check})"));
            } else if msg == "Already downloaded" {
                self.hooks.log("info", &format!("⏳ Skipping existing file: '{name}' (already present on disk)"));
            } else if msg == "Already archived" {
                self.hooks.log("info", &format!("📦 Skipping archived file: '{name}' (present in download archive)"));
            } else if let Some(why) = msg.strip_prefix("Skipped:") {
                self.hooks.log("info", &format!("⏭️ Skipping {name}: {}", why.trim()));
            }
        } else if msg.starts_with("429_RATE_LIMIT") {
            self.hooks.log("warning", &format!("⚠ {name}: HTTP 429 Too Many Requests (Rate limited)"));
        } else if !["Cancelled", "Batch paused by user", "Paused"].contains(&msg) {
            self.hooks.log("warning", &format!("✖ {name}: {msg}"));
        }
    }

    /// What happens when a file's download ends
    fn file_done(&self, c: &Cell, ok: bool, msg: &str, successes: &mut usize, locked: bool) {
        self.log_result(c, ok, msg);
        let task = c.lock().unwrap().task.clone();
        if ok {
            let skipped = {
                let mut it = c.lock().unwrap();
                if it.st.status == "skipped" || msg.starts_with("Skipped:") {
                    it.st.status = "skipped".into();
                    if it.st.error_msg.is_empty() {
                        if let Some(r) = msg.strip_prefix("Skipped:") {
                            it.st.error_msg = r.trim().into();
                        }
                    }
                    it.st.progress_pct = 100;
                    it.st.eta_str = "Skipped".into();
                    true
                } else {
                    it.st.status = "completed".into();
                    it.st.error_msg.clear();
                    false
                }
            };
            self.write_pending_post_info(&task);
            if !skipped {
                *successes += 1;
                self.run.lock().unwrap().stable_clean += 1;
                self.hooks.file_completed(&task);
                let (used_preview, size) = {
                    let it = c.lock().unwrap();
                    (it.st.used_preview, it.task.file_size.max(0) as u64)
                };
                if self.hooks.archive_enabled() && !used_preview {
                    self.hooks.record_file(&task, size, &task.target_path);
                }
            }
            self.hooks.changed(c);
            return;
        }
        *successes = 0;
        self.run.lock().unwrap().stable_clean = 0;
        let mut it = c.lock().unwrap();
        if msg.starts_with("429_RATE_LIMIT") {
            let wait = if locked { 30 } else { 15 };
            it.st.status = "pending".into();
            it.st.error_msg = format!("Rate limited (429) — cooling down for {wait}s before auto-retry");
            drop(it);
            self.hooks.changed(c);
            self.rate_limit_backoff(locked);
        } else if msg.contains("Disk full") {
            it.st.status = "pending".into();
            it.st.error_msg = msg.into();
            drop(it);
            self.hooks.changed(c);
            if !self.ctl.paused() {
                self.pause();
                self.hooks.log("warning", "⚠️ [Disk Full Alert] Destination drive ran out of space! Downloads paused to prevent file corruption. Please free space on your drive and click Resume.");
            }
        } else if it.st.status == "cancelled" {
            // Cancelled while it was downloading: it stays cancelled
            if it.st.error_msg.is_empty() {
                it.st.error_msg = "Download cancelled by user".into();
            }
            it.st.stop_moving();
            it.st.eta_str = "--".into();
            drop(it);
            self.hooks.changed(c);
        } else if msg.contains("Batch paused by user") || it.st.status == "paused" {
            it.st.status = "paused".into();
            it.st.error_msg.clear();
            it.st.stop_moving();
            it.st.eta_str = "Paused".into();
            drop(it);
            self.hooks.changed(c);
        } else {
            it.st.status = "failed".into();
            it.st.error_msg = msg.into();
            drop(it);
            self.hooks.changed(c);
        }
    }

    /// "Auto retry at the end": failed files go back in line (5 times at most); true when some did
    fn auto_retry(&self, o: &Options) -> bool {
        let skip_404 = o.flag("skip_retry_404");
        let mut list = Vec::new();
        for c in self.cells() {
            let mut it = c.lock().unwrap();
            if it.st.status != "failed" || (skip_404 && is_404(&it)) {
                continue;
            }
            let err = it.st.error_msg.to_lowercase();
            if FATAL_AUTH.iter().any(|f| err.contains(f)) {
                it.st.retry_capped = true;
                continue;
            }
            if it.st.retry_count < 5 {
                drop(it);
                list.push(c);
            } else {
                it.st.retry_capped = true;
                let orig = if it.st.error_msg.is_empty() { "Download failed".to_string() } else { it.st.error_msg.clone() };
                if !orig.contains("(Stopped after 5 retries)") {
                    it.st.error_msg = format!("{orig} (Stopped after 5 retries)");
                }
                drop(it);
                self.hooks.changed(&c);
            }
        }
        if list.is_empty() {
            return false;
        }
        let attempt = list[0].lock().unwrap().st.retry_count + 1;
        let wait = (1.5 * attempt as f64).min(6.0);
        self.hooks.log("info", &format!("🔄 Auto-retry triggered for {} failed files (attempt {attempt}/5, cooling down {wait:.1}s)...", list.len()));
        for c in &list {
            Self::reset_for_retry(&mut c.lock().unwrap());
            self.hooks.changed(c);
        }
        std::thread::sleep(Duration::from_secs_f64(wait));
        true
    }

    fn update_adaptive_state(&self, adaptive: bool, locked: bool, target_max: usize) {
        let t = now();
        let mut r = self.run.lock().unwrap();
        let w = r.max_workers;
        let rem = ((r.cooldown_until - t) as i64).max(1);
        let (state, text) = if locked {
            if t < r.cooldown_until { ("cooldown", format!("Cooldown: {rem}s (Locked {w} threads)")) } else { ("locked", format!("Locked ({w} threads)")) }
        } else if adaptive {
            if t < r.cooldown_until {
                ("cooldown", format!("Cooldown: {rem}s ({w} threads)"))
            } else if r.learned_ceiling.is_some_and(|c| w >= c) {
                ("optimal", format!("Stable Lock ({w} threads)"))
            } else if w >= target_max {
                ("optimal", format!("Optimal ({w} threads)"))
            } else {
                ("scaling", format!("Scaling ({w}/{} threads)", r.learned_ceiling.unwrap_or(target_max)))
            }
        } else {
            ("manual", format!("Manual ({w} threads)"))
        };
        r.adaptive_state = state.into();
        r.adaptive_text = text;
    }

    fn finish_run(&self) {
        self.run.lock().unwrap().running = false;
        let elapsed = self.active_elapsed(now());
        let counts = self.status_counts();
        let get = |k: &str| counts.get(k).copied().unwrap_or(0);
        let (completed, failed, skipped) = (get("completed"), get("failed"), get("skipped"));
        self.emit_progress(true);

        // Post texts of files that won't be downloaded any more are dropped
        let live: HashSet<(String, String)> = self
            .cells()
            .iter()
            .filter_map(|c| {
                let it = c.lock().unwrap();
                (!it.task.post_info_path.is_empty() && !["completed", "skipped"].contains(&it.st.status.as_str())).then(|| (it.task.post_info_path.clone(), it.task.post_id.clone()))
            })
            .collect();
        self.post_infos.lock().unwrap().retain(|k, _| live.contains(k));

        if self.ctl.cancelled() {
            self.hooks.discard_recovery();
            self.hooks.log("warning", &format!("Download cancelled. Completed: {completed}, Skipped: {skipped}, Failed/Cancelled: {failed}"));
            self.hooks.finished(false, "Download cancelled by user.");
            return;
        }
        // A recovery journal only for files that never got their turn
        if self.cells().iter().any(|c| ACTIVE.contains(&c.lock().unwrap().st.status.as_str())) {
            self.hooks.checkpoint();
        } else {
            self.hooks.discard_recovery();
        }
        let skip_msg = if skipped > 0 { format!(", {skipped} skipped") } else { String::new() };
        let run_tasks = std::mem::take(&mut self.run.lock().unwrap().run_tasks);
        let total = self.items.lock().unwrap().len();
        let mut note = String::new();
        if !run_tasks.is_empty() && run_tasks.len() < total {
            let ok = run_tasks.iter().filter(|c| ["completed", "skipped"].contains(&c.lock().unwrap().st.status.as_str())).count();
            note = format!(" This run: {ok} of {} file(s) succeeded.", run_tasks.len());
        }
        let already = self.ctl.already_there.lock().unwrap().clone();
        if !already.is_empty() {
            let mut v: Vec<_> = already.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            note += &format!(" Found downloaded already: {}.", v.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", "));
        }
        self.hooks.log("success", &format!("Download completed in {elapsed:.1}s! ({completed} successful{skip_msg}, {failed} errors){note}"));
        self.hooks.finished(true, &format!("Completed: {completed} downloaded{skip_msg}, {failed} failed."));
    }

    // ── Speed, ETA, progress ─────────────────────────────────────────────────

    fn active_elapsed(&self, t: f64) -> f64 {
        let r = self.run.lock().unwrap();
        let m = &r.meter;
        if m.start <= 0.0 {
            return 0.1;
        }
        let mut paused = m.paused_total;
        if self.ctl.paused() && m.pause_start > 0.0 {
            paused += (t - m.pause_start).max(0.0);
        }
        ((t - m.start) - paused).max(0.1)
    }

    fn instant_speed(m: &mut Meter, t: f64, bytes: u64) -> u64 {
        if bytes > 0 && m.first_byte == 0.0 {
            m.first_byte = t;
        }
        if m.samples.back().is_some_and(|s| t - s.0 > 2.0) {
            m.samples.clear();
            m.smoothed = 0.0;
        }
        m.samples.push_back((t, bytes));
        while m.samples.front().is_some_and(|s| t - s.0 > 2.0) {
            m.samples.pop_front();
        }
        if m.rolling.back().is_none_or(|s| t - s.0 >= 0.5) {
            m.rolling.push_back((t, bytes));
        }
        while m.rolling.front().is_some_and(|s| t - s.0 > 45.0) {
            m.rolling.pop_front();
        }
        if m.samples.len() >= 2 {
            let (t0, b0) = m.samples[0];
            let dt = t - t0;
            if dt >= 0.2 {
                let raw = (bytes.saturating_sub(b0) as f64 / dt).max(0.0);
                m.smoothed = if m.smoothed <= 0.0 { raw } else { 0.85 * raw + 0.15 * m.smoothed };
            }
        }
        m.smoothed.max(0.0) as u64
    }

    fn stats(&self, t: f64) -> Stats {
        if let Some((when, s)) = self.run.lock().unwrap().meter.stats {
            if t - when < 1.0 {
                return s;
            }
        }
        const VIDEO_ARCHIVE: &[&str] = &[".mp4", ".m4v", ".mkv", ".webm", ".mov", ".avi", ".wmv", ".zip", ".rar", ".7z", ".tar", ".gz", ".bz2", ".xz", ".iso"];
        const IMAGE_AUDIO: &[&str] = &[".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".svg", ".ico", ".mp3", ".flac", ".wav", ".m4a", ".ogg", ".opus"];
        let mut s = Stats::default();
        for c in self.cells() {
            let it = c.lock().unwrap();
            let d = it.st.downloaded_bytes;
            s.all_downloaded += d;
            let name = basename(&it.task.target_path).to_lowercase();
            let ext = name.rfind('.').map(|i| name[i..].split('?').next().unwrap_or("").to_string()).unwrap_or_default();
            let (va, ia) = (VIDEO_ARCHIVE.contains(&ext.as_str()), IMAGE_AUDIO.contains(&ext.as_str()));
            match it.st.status.as_str() {
                "completed" if d > 0 => {
                    s.done_bytes += d as f64;
                    s.done_n += 1.0;
                    if va {
                        s.done_video_bytes += d as f64;
                        s.done_video_n += 1.0;
                    } else if ia {
                        s.done_image_bytes += d as f64;
                        s.done_image_n += 1.0;
                    }
                }
                "pending" | "downloading" => {
                    let size = it.task.file_size;
                    if size > 0 {
                        s.known_bytes += size as f64;
                        s.known_n += 1.0;
                        s.known_remaining += (size as f64 - d as f64).max(0.0);
                    } else {
                        s.unknown_n += 1.0;
                        s.unknown_downloaded += d as f64;
                        if va {
                            s.unknown_video_n += 1.0;
                        } else if ia {
                            s.unknown_image_n += 1.0;
                        } else {
                            s.unknown_other_n += 1.0;
                        }
                    }
                }
                _ => {}
            }
        }
        self.run.lock().unwrap().meter.stats = Some((t, s));
        s
    }

    fn smart_eta(&self, completed: usize, failed: usize, total: usize, speed: u64, elapsed: f64) -> String {
        let remaining = total as i64 - completed as i64 - failed as i64;
        if total == 0 || remaining <= 0 {
            self.run.lock().unwrap().meter.smoothed_eta = None;
            return "Done".into();
        }
        let t = now();
        let st = self.stats(t);
        let bytes = self.ctl.bytes.load(Ordering::Relaxed) as f64;
        let mut r = self.run.lock().unwrap();
        let paused_now = self.ctl.paused();
        let m = &mut r.meter;
        let since = if m.last_eta_calc > 0.0 { t - m.last_eta_calc } else { 0.0 };
        m.last_eta_calc = t;

        let clamp = |v: f64, lo: f64, hi: f64| v.max(lo).min(hi);
        let avg_video = clamp(if st.done_video_n > 0.0 { st.done_video_bytes / st.done_video_n } else { 60.0 * 1048576.0 }, 1048576.0, 1024.0 * 1048576.0);
        let avg_image = clamp(if st.done_image_n > 0.0 { st.done_image_bytes / st.done_image_n } else { 3.5 * 1048576.0 }, 100.0 * 1024.0, 50.0 * 1048576.0);
        let avg_other = clamp(
            if st.done_n > 0.0 { st.done_bytes / st.done_n } else if st.known_n > 0.0 { st.known_bytes / st.known_n } else { 4.5 * 1048576.0 },
            200.0 * 1024.0,
            100.0 * 1048576.0,
        );
        let unknown_bytes = st.unknown_video_n * avg_video + st.unknown_image_n * avg_image + st.unknown_other_n.max(0.0) * avg_other;
        let remaining_bytes = st.known_remaining + (unknown_bytes - st.unknown_downloaded).max(0.0);

        let mut paused = m.paused_total;
        if paused_now && m.pause_start > 0.0 {
            paused += (t - m.pause_start).max(0.0);
        }
        let transfer_elapsed = if m.first_byte > 0.0 && m.start > 0.0 { elapsed.min(((t - m.first_byte) - paused).max(0.5)) } else { elapsed.max(0.5) };
        let session_avg = bytes / transfer_elapsed.max(1.0);
        let rolling = if m.rolling.len() >= 2 {
            let (t0, b0) = m.rolling[0];
            let dt = t - t0;
            if dt >= 3.0 { ((bytes - b0 as f64) / dt).max(0.0) } else { 0.0 }
        } else {
            0.0
        };
        let sustained = if rolling > 0.0 { rolling } else { session_avg };
        m.medium = if m.medium <= 0.0 { if speed > 0 { speed as f64 } else { sustained } } else { 0.05 * speed as f64 + 0.95 * m.medium };
        let learn = ((transfer_elapsed - 5.0) / 60.0).clamp(0.20, 0.85);
        let eta_speed = (1.0 - learn) * m.medium + learn * sustained;

        let mut cands = Vec::new();
        if eta_speed > 512.0 && remaining_bytes > 0.0 {
            cands.push(remaining_bytes / eta_speed);
        }
        if completed > 0 && transfer_elapsed > 2.0 {
            let rate = completed as f64 / transfer_elapsed;
            if rate > 0.0 {
                cands.push(remaining as f64 / rate);
            }
        }
        if cands.is_empty() {
            return "--".into();
        }
        let target = if cands.len() == 2 {
            let active = st.known_n + st.unknown_n;
            let conf = if active > 0.0 { st.known_n / active } else if st.known_bytes > 0.0 { 1.0 } else { 0.5 };
            let w = 0.55 + 0.35 * conf;
            w * cands[0] + (1.0 - w) * cands[1]
        } else {
            cands[0]
        };
        let eta = match m.smoothed_eta {
            None => target,
            Some(mut e) => {
                if since > 0.0 && since < 3.0 {
                    e = (e - since).max(1.0);
                }
                let dev = (target - e).abs() / e.max(1.0);
                let alpha = if transfer_elapsed < 15.0 {
                    if dev > 0.25 { 0.20 } else { 0.10 }
                } else if target < 10.0 && e > 20.0 {
                    0.15
                } else if dev > 0.60 {
                    0.10
                } else if dev > 0.25 {
                    0.04
                } else {
                    0.015
                };
                (1.0 - alpha) * e + alpha * target
            }
        };
        m.smoothed_eta = Some(eta);
        let s = (eta.round() as i64).max(1);
        if s >= 86400 {
            format!("{}d {}h", s / 86400, (s % 86400) / 3600)
        } else if s >= 3600 {
            format!("{}h {}m {}s", s / 3600, (s % 3600) / 60, s % 60)
        } else if s >= 60 {
            format!("{}m {}s", s / 60, s % 60)
        } else if s > 1 {
            format!("{s}s")
        } else {
            "< 1s".into()
        }
    }

    /// The overall progress the window shows
    pub fn progress_info(&self) -> Value {
        let t = now();
        let counts = self.status_counts();
        let get = |k: &str| counts.get(k).copied().unwrap_or(0);
        let completed = get("completed") + get("skipped");
        let failed = get("failed");
        let cells = self.cells();
        let total = cells.len();
        let elapsed = self.active_elapsed(t);
        let bytes = self.ctl.bytes.load(Ordering::Relaxed);
        let speed = Self::instant_speed(&mut self.run.lock().unwrap().meter, t, bytes);
        let e = elapsed as u64;
        let elapsed_str = if e >= 3600 { format!("{}h {}m {}s", e / 3600, (e % 3600) / 60, e % 60) } else if e >= 60 { format!("{}m {}s", e / 60, e % 60) } else { format!("{e}s") };
        let eta = self.smart_eta(completed, failed, total, speed, elapsed);
        let percent = if total > 0 {
            let in_flight: f64 = cells
                .iter()
                .map(|c| {
                    let it = c.lock().unwrap();
                    if it.st.status == "downloading" && it.task.file_size > 0 { (it.st.downloaded_bytes as f64 / it.task.file_size as f64).min(1.0) } else { 0.0 }
                })
                .sum();
            (((completed + failed) as f64 + in_flight) / total as f64 * 100.0).clamp(0.0, 100.0) * 10.0
        } else {
            0.0
        };
        let effective = bytes.max(self.stats(t).all_downloaded);
        let saved = if effective > 1 << 30 { format!("{:.2} GB", effective as f64 / (1u64 << 30) as f64) } else { format!("{:.1} MB", effective as f64 / 1048576.0) };
        let r = self.run.lock().unwrap();
        let status = if self.ctl.paused() { "Progress: Paused".to_string() } else if r.running { format!("Downloading… ({completed}/{total} files)") } else { "Idle".into() };
        json!({
            "completed": completed, "failed": failed, "total": total, "percent": percent.round() / 10.0,
            "speed_str": format_speed(speed), "eta_str": eta, "elapsed_str": elapsed_str, "downloaded_bytes": bytes,
            "saved_str": saved, "status_text": status, "adaptive_state": r.adaptive_state, "adaptive_status_text": r.adaptive_text,
        })
    }

    fn emit_progress(&self, force: bool) {
        let t = now();
        {
            let mut r = self.run.lock().unwrap();
            if !force && t - r.meter.last_emit < 0.2 {
                return;
            }
            r.meter.last_emit = t;
        }
        let info = self.progress_info();
        self.hooks.progress(&info);
    }
}

/// The folder of a queued file (for the log)
pub fn folder_of(t: &Task) -> String {
    dirname(&t.target_path)
}
