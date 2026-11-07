//! Lifecycle of the Python engine process.
//!
//! Lookup order for what to run:
//!   1. `PAWCHIVE_ENGINE` env var: a ready-built engine executable
//!   2. `pawchive-engine(.exe)` next to the app executable (release bundle, built with PyInstaller)
//!   3. `PAWCHIVE_PYTHON` env var, or the repository's `.venv` interpreter, running `-m server.sidecar`
//!      from the repository root (development)

use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Serialize)]
pub struct EngineInfo {
    pub port: u16,
    pub token: String,
    /// Where the engine keeps its data and settings (installed: AppData; portable: next to the
    /// app). The Rust parts use the same files.
    #[serde(skip)]
    pub data_dir: std::path::PathBuf,
    #[serde(skip)]
    pub config_dir: std::path::PathBuf,
}

enum Status {
    Starting,
    Ready(EngineInfo),
    Failed(String),
}

struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    group: Option<crate::proc_group::Group>,
}

pub struct Engine {
    status: Mutex<Status>,
    ready: Condvar,
    running: Mutex<Option<Running>>,
    log_dir: Option<PathBuf>,
    env: Mutex<Vec<(String, String)>>,
}

const READY_PREFIX: &str = "PAWCHIVE_SIDECAR_READY ";

impl Engine {
    pub fn new(log_dir: Option<PathBuf>) -> Self {
        Self {
            status: Mutex::new(Status::Starting),
            ready: Condvar::new(),
            running: Mutex::new(None),
            log_dir,
            env: Mutex::new(Vec::new()),
        }
    }

    /// Extra environment for the engine process (e.g. where the download engine listens).
    pub fn set_env(&self, key: &str, value: &str) {
        self.env.lock().unwrap().push((key.to_string(), value.to_string()));
    }

    fn repo_root() -> PathBuf {
        // desktop/src-tauri → repository root
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    fn command(&self) -> Result<Command, String> {
        let exe_name = if cfg!(windows) { "pawchive-engine.exe" } else { "pawchive-engine" };
        if let Ok(p) = std::env::var("PAWCHIVE_ENGINE") {
            return Ok(Command::new(p));
        }
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            for candidate in [dir.join(exe_name), dir.join("engine").join(exe_name)] {
                if candidate.is_file() {
                    let mut c = Command::new(&candidate);
                    if let Some(parent) = candidate.parent() {
                        c.current_dir(parent);
                    }
                    return Ok(c);
                }
            }
        }
        let root = Self::repo_root();
        let python = std::env::var("PAWCHIVE_PYTHON").map(PathBuf::from).unwrap_or_else(|_| {
            if cfg!(windows) {
                root.join(".venv").join("Scripts").join("python.exe")
            } else {
                root.join(".venv").join("bin").join("python")
            }
        });
        if !python.is_file() {
            return Err(format!(
                "No engine found. Build pawchive-engine, or create the Python venv at {}",
                python.display()
            ));
        }
        let mut c = Command::new(python);
        c.current_dir(&root).args(["-X", "utf8", "-m", "server.sidecar"]);
        Ok(c)
    }

    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        *self.status.lock().unwrap() = Status::Starting;
        // A fixed port / token can be given for development tooling (e.g. driving the same engine
        // from a browser); normally both are chosen fresh on every start.
        let token = std::env::var("PAWCHIVE_ENGINE_TOKEN").ok().filter(|t| t.len() >= 3).unwrap_or_else(new_token);
        let port = std::env::var("PAWCHIVE_ENGINE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(0);
        let mut cmd = self.command()?;
        cmd.args(["--port", &port.to_string(), "--token", &token])
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .envs(self.env.lock().unwrap().clone())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW, and suspended until it's inside its job object (see proc_group.rs)
            cmd.creation_flags(0x0800_0000 | crate::proc_group::CREATE_SUSPENDED);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| format!("Couldn't start the engine: {e}"))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdin = child.stdin.take();
        let group = crate::proc_group::adopt(&child);
        *self.running.lock().unwrap() = Some(Running { child, stdin, group });

        // stderr → log file (also keeps the pipe drained)
        let log_path = self.log_dir.as_ref().map(|d| {
            let _ = std::fs::create_dir_all(d);
            d.join("engine.log")
        });
        std::thread::spawn(move || {
            let mut file = log_path.and_then(|p| std::fs::File::create(p).ok());
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(f) = file.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
            }
        });

        // stdout: wait for the ready line, then keep draining
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            let mut announced = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if !announced {
                    if let Some(json) = line.strip_prefix(READY_PREFIX) {
                        let ready = serde_json::from_str::<serde_json::Value>(json).unwrap_or_default();
                        let port = ready.get("port").and_then(|p| p.as_u64()).unwrap_or(0) as u16;
                        let dir = |k: &str| std::path::PathBuf::from(ready.get(k).and_then(|v| v.as_str()).unwrap_or(""));
                        this.set_status(if port > 0 {
                            Status::Ready(EngineInfo {
                                port,
                                token: token.clone(),
                                data_dir: dir("dataDir"),
                                config_dir: dir("configDir"),
                            })
                        } else {
                            Status::Failed("The engine started without a port".into())
                        });
                        announced = true;
                    }
                }
            }
            if !announced {
                this.set_status(Status::Failed(
                    "The engine stopped during start-up. See engine.log in the app's log folder.".into(),
                ));
            }
        });
        Ok(())
    }

    fn set_status(&self, s: Status) {
        *self.status.lock().unwrap() = s;
        self.ready.notify_all();
    }

    pub fn wait_ready(&self, timeout: Duration) -> Result<EngineInfo, String> {
        let deadline = Instant::now() + timeout;
        let mut status = self.status.lock().unwrap();
        loop {
            match &*status {
                Status::Ready(info) => return Ok(info.clone()),
                Status::Failed(e) => return Err(e.clone()),
                Status::Starting => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err("The engine is taking too long to start".into());
                    }
                    status = self.ready.wait_timeout(status, deadline - now).unwrap().0;
                }
            }
        }
    }

    /// Closing stdin asks the engine to quit cleanly (it saves the session); the whole process
    /// tree is killed if it lingers. Dropping the job handle also ends anything left behind.
    pub fn stop(&self) {
        let Some(mut run) = self.running.lock().unwrap().take() else { return };
        drop(run.stdin.take());
        // The engine saves the session and lets download threads finish; it has its own 20 s
        // watchdog, so give it a little longer than that before ending the tree
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = run.child.try_wait() {
                return; // `run` drops here: closing the job ends any stragglers
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        match &run.group {
            Some(group) => group.kill(),
            None => {
                let _ = run.child.kill();
            }
        }
        let _ = run.child.wait();
    }

    pub fn restart(self: &Arc<Self>) -> Result<(), String> {
        self.stop();
        self.start()
    }
}

pub fn new_token() -> String {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("OS random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
