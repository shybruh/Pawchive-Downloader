//! The download engine: transfers files for the Python downloader (and later the Rust one).
//!
//! A plain local socket. The caller sends one line, a JSON object with the token and a
//! `download::Job`; the engine answers one JSON line per update: progress
//! `{"t":"p","done":…,"total":…}` about four times a second, then
//! `{"t":"done","ok":true,"size":…,"sha256":"…","resumedFrom":…}` or
//! `{"t":"done","ok":false,"kind":"http|network|disk|cancelled|other","status":…,"error":"…"}`.
//! Closing the connection stops the transfer; what's on disk is kept for the next try.

mod download;
mod limiter;

use download::{Job, Progress};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct TransferInfo {
    pub url: String,
    pub token: String,
}

pub fn start() -> Result<TransferInfo, String> {
    // A fixed port / token can be given for development tooling, like the engine's and the host's
    let fixed: u16 = std::env::var("PAWCHIVE_TRANSFER_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", fixed)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = std::env::var("PAWCHIVE_TRANSFER_TOKEN").ok().filter(|t| t.len() >= 3).unwrap_or_else(crate::engine::new_token);
    let t = token.clone();
    std::thread::Builder::new()
        .name("transfer-accept".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let t = t.clone();
                std::thread::spawn(move || serve(stream, &t));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(TransferInfo { url: format!("tcp://127.0.0.1:{port}"), token })
}

fn line(stream: &mut TcpStream, v: Value) -> bool {
    let mut b = v.to_string().into_bytes();
    b.push(b'\n');
    stream.write_all(&b).and_then(|_| stream.flush()).is_ok()
}

fn serve(mut stream: TcpStream, token: &str) {
    let _ = stream.set_nodelay(true);
    let mut first = String::new();
    {
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });
        if reader.read_line(&mut first).is_err() {
            return;
        }
    }
    let Ok(req) = serde_json::from_str::<Value>(&first) else { return };
    if req.get("token").and_then(Value::as_str) != Some(token) {
        return;
    }
    let Some(job) = req.get("job").cloned().and_then(|j| serde_json::from_value::<Job>(j).ok()) else {
        let _ = line(&mut stream, json!({"t": "done", "ok": false, "kind": "other", "status": 0, "error": "bad request"}));
        return;
    };

    let p = Arc::new(Progress { done: AtomicU64::new(0), total: AtomicU64::new(job.total), cancel: AtomicBool::new(false) });
    // The caller hanging up (a read that ends) stops the transfer
    if let Ok(watch) = stream.try_clone() {
        let p = p.clone();
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = BufReader::new(watch).read_line(&mut buf);
            p.cancel.store(true, Ordering::Relaxed);
        });
    }
    let (tx, rx) = channel();
    {
        let p = p.clone();
        std::thread::spawn(move || {
            let _ = tx.send(download::run(&job, &p));
        });
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(result) => {
                let v = match result {
                    Ok(d) => json!({"t": "done", "ok": true, "size": d.size, "sha256": d.sha256, "resumedFrom": d.resumed_from}),
                    Err(f) => json!({"t": "done", "ok": false, "kind": f.kind(), "status": f.status(), "error": f.message()}),
                };
                let _ = line(&mut stream, v);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                let v = json!({"t": "p", "done": p.done.load(Ordering::Relaxed), "total": p.total.load(Ordering::Relaxed)});
                if !line(&mut stream, v) {
                    p.cancel.store(true, Ordering::Relaxed);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
