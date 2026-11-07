//! The engine host: the UI's one connection to the engine.
//!
//! The UI connects here (a local WebSocket with its own secret). For every UI connection the host
//! opens a matching connection to the Python engine. Requests for objects implemented in Rust
//! (`native`) are answered here; everything else is passed through to Python untouched, and
//! Python's answers and events flow back the same way. `describe` gets the Rust objects merged
//! in, so the UI sees one engine.
//!
//! Porting a part of the engine means adding its object to `natives()` (and, once nothing in
//! Python still needs it, deleting the Python side).

pub mod native;
pub mod queue;

use crate::engine::Engine;
use crate::media::MediaInfo;
use futures_util::{SinkExt, StreamExt};
use native::{Events, NativeObject, Natives, Shell};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, serde::Serialize)]
pub struct HostInfo {
    pub port: u16,
    pub token: String,
}

pub struct Host {
    info: HostInfo,
}

/// What the host does with the Python engine's answer to one of the UI's requests.
enum Patch {
    /// `describe`: add the Rust objects
    Describe,
    /// `info`: the media server (Rust's) instead of Python's
    Info,
}

/// The engine objects implemented in Rust. Their names hide the Python objects of the same name.
fn natives(app_version: &str, engine: &Arc<Engine>, events: &Events) -> Natives {
    let shell: Arc<dyn NativeObject> = Arc::new(Shell { version: app_version.to_string() });
    let accounts: Arc<dyn NativeObject> = Arc::new(crate::accounts::Accounts::new(engine.clone(), events.clone()));
    let mut objects = vec![("shell", shell), ("accounts", accounts)];
    // The download queue's view, kept by the app's Rust queue model (not with the old Python downloader)
    if std::env::var("PAWCHIVE_PYTHON_DOWNLOADER").ok().as_deref() != Some("1") {
        let q: Arc<dyn NativeObject> = Arc::new(queue::QueueModel);
        let g: Arc<dyn NativeObject> = Arc::new(queue::GroupsModel);
        objects.push((queue::QUEUE, q));
        objects.push((queue::GROUPS, g));
        queue::start_pump(events.clone());
    }
    Natives::new(objects)
}

impl Host {
    /// Starts listening. A fixed port / secret can be given for development tooling
    /// (PAWCHIVE_HOST_PORT / PAWCHIVE_HOST_TOKEN), like the engine's own.
    pub fn start(engine: Arc<Engine>, media: MediaInfo, app_version: String) -> Result<Arc<Host>, String> {
        let port: u16 = std::env::var("PAWCHIVE_HOST_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
        let token = std::env::var("PAWCHIVE_HOST_TOKEN")
            .ok()
            .filter(|t| t.len() >= 3)
            .unwrap_or_else(crate::engine::new_token);

        let std_listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        std_listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = std_listener.local_addr().map_err(|e| e.to_string())?.port();
        let host = Arc::new(Host { info: HostInfo { port, token: token.clone() } });

        let events = Events::new();
        let natives = natives(&app_version, &engine, &events);
        tauri::async_runtime::spawn(async move {
            let listener = match TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[host] couldn't listen: {e}");
                    return;
                }
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let (engine, natives, events, token, media) =
                    (engine.clone(), natives.clone(), events.clone(), token.clone(), media.clone());
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = serve(stream, engine, natives, events, token, media).await {
                        eprintln!("[host] connection ended: {e}");
                    }
                });
            }
        });
        Ok(host)
    }

    pub fn info(&self) -> HostInfo {
        self.info.clone()
    }
}

/// One UI connection, and its matching connection to the Python engine.
async fn serve(
    stream: TcpStream,
    engine: Arc<Engine>,
    natives: Natives,
    events: Events,
    token: String,
    media: MediaInfo,
) -> Result<(), String> {
    let check = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let ok = req.uri().query().map(|q| q.split('&').any(|kv| kv == format!("token={token}"))).unwrap_or(false);
        if ok {
            Ok(resp)
        } else {
            let mut r = ErrorResponse::new(None);
            *r.status_mut() = StatusCode::FORBIDDEN;
            Err(r)
        }
    };
    let ui = tokio_tungstenite::accept_hdr_async(stream, check).await.map_err(|e| e.to_string())?;
    let (mut ui_tx, mut ui_rx) = ui.split();

    // The Python engine (it may still be starting)
    let py = {
        let engine = engine.clone();
        tauri::async_runtime::spawn_blocking(move || engine.wait_ready(std::time::Duration::from_secs(60)))
            .await
            .map_err(|e| e.to_string())??
    };
    let (upstream, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/?token={}", py.port, py.token))
        .await
        .map_err(|e| format!("couldn't reach the Python engine: {e}"))?;
    let (mut py_tx, mut py_rx) = upstream.split();

    // Everything for the UI goes through one queue (answers, Python's events, Rust's events)
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tauri::async_runtime::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if ui_tx.send(Message::text(text)).await.is_err() {
                break;
            }
        }
    });

    let mut rust_events = events.subscribe();
    let ev_out = out_tx.clone();
    let event_pump = tauri::async_runtime::spawn(async move {
        while let Ok(text) = rust_events.recv().await {
            if ev_out.send(text).is_err() {
                break;
            }
        }
    });

    let patches: Arc<Mutex<HashMap<i64, Patch>>> = Arc::default();

    // Python → UI
    let (py_out, py_patches, py_natives) = (out_tx.clone(), patches.clone(), natives.clone());
    let from_python = tauri::async_runtime::spawn(async move {
        while let Some(Ok(msg)) = py_rx.next().await {
            let Message::Text(text) = msg else { continue };
            let text = text.to_string();
            if hidden_event(&text, &py_natives) {
                continue;
            }
            let patched = patch_answer(&text, &py_patches, &py_natives, &media);
            if py_out.send(patched.unwrap_or(text)).is_err() {
                break;
            }
        }
    });

    // UI → Rust objects or Python
    while let Some(Ok(msg)) = ui_rx.next().await {
        let Message::Text(text) = msg else { continue };
        let text = text.to_string();
        let Ok(req) = serde_json::from_str::<Value>(&text) else { continue };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let op = req.get("op").and_then(Value::as_str).unwrap_or("");
        let target = req.get("obj").and_then(Value::as_str).unwrap_or("");

        if let Some(obj) = natives.get(target) {
            let reply = match natives.handle(obj, &req) {
                Ok(result) => json!({"id": id, "ok": true, "result": result}),
                Err(error) => json!({"id": id, "ok": false, "error": error}),
            };
            let _ = out_tx.send(reply.to_string());
            continue;
        }
        if let Some(n) = id.as_i64() {
            match op {
                "describe" => {
                    patches.lock().unwrap().insert(n, Patch::Describe);
                }
                "info" => {
                    patches.lock().unwrap().insert(n, Patch::Info);
                }
                _ => {}
            }
        }
        if py_tx.send(Message::text(text)).await.is_err() {
            break;
        }
    }

    from_python.abort();
    event_pump.abort();
    writer.abort();
    Ok(())
}

/// A Python event about the state of an object Rust answers for (its own events say that);
/// Python's signals still pass (the app runs the object's actions)
fn hidden_event(text: &str, natives: &Natives) -> bool {
    if !(text.starts_with("{\"event\": \"props\"") || text.starts_with("{\"event\": \"model\"")) {
        return false;
    }
    let Ok(msg) = serde_json::from_str::<Value>(text) else { return false };
    msg.get("obj").and_then(Value::as_str).is_some_and(|o| natives.get(o).is_some())
}

/// Adds what the host knows to Python's answers to `describe` and `info`.
fn patch_answer(text: &str, patches: &Mutex<HashMap<i64, Patch>>, natives: &Natives, media: &MediaInfo) -> Option<String> {
    // Answers start with {"id": …; events don't. Only parse what may need patching.
    if !text.starts_with("{\"id\"") {
        return None;
    }
    let mut msg: Value = serde_json::from_str(text).ok()?;
    let id = msg.get("id")?.as_i64()?;
    let patch = patches.lock().unwrap().remove(&id)?;
    let result = msg.get_mut("result")?.as_object_mut()?;
    match patch {
        Patch::Describe => {
            for (name, entry) in natives.describe_all() {
                result.insert(name, entry);
            }
        }
        Patch::Info => {
            result.insert("mediaPort".into(), json!(media.port));
            result.insert("mediaToken".into(), json!(media.token));
        }
    }
    Some(msg.to_string())
}
