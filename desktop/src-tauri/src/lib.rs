//! Pawchive desktop shell.
//!
//! Starts the Python engine (`server/sidecar.py`) as a background process, hands its WebSocket
//! port and secret token to the React UI, and stops the engine when the app exits.

mod engine;
mod proc_group;
mod site_login;

use engine::{Engine, EngineInfo};
use std::sync::Arc;
use tauri::{Manager, RunEvent, State};

// Both commands wait on the engine process, so they run off the main (window) thread.
#[tauri::command]
async fn engine_info(engine: State<'_, Arc<Engine>>) -> Result<EngineInfo, String> {
    let engine = Arc::clone(&engine);
    tauri::async_runtime::spawn_blocking(move || engine.wait_ready(std::time::Duration::from_secs(60)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn engine_restart(engine: State<'_, Arc<Engine>>) -> Result<(), String> {
    let engine = Arc::clone(&engine);
    tauri::async_runtime::spawn_blocking(move || engine.restart())
        .await
        .map_err(|e| e.to_string())?
}

/// Stops the engine before an update replaces its files (the installer can't overwrite them while
/// it runs).
#[tauri::command]
async fn engine_shutdown(engine: State<'_, Arc<Engine>>) -> Result<(), String> {
    let engine = Arc::clone(&engine);
    tauri::async_runtime::spawn_blocking(move || engine.stop())
        .await
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let log_dir = app.path().app_log_dir().ok();
            let engine = Arc::new(Engine::new(log_dir));
            engine.start()?;
            app.manage(engine);
            app.manage(site_login::LoginSite::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_info,
            engine_restart,
            engine_shutdown,
            site_login::site_login_open,
            site_login::site_login_done,
            site_login::site_login_cancel
        ])
        .build(tauri::generate_context!())
        .expect("error while building the Pawchive shell");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(engine) = handle.try_state::<Arc<Engine>>() {
                engine.stop();
            }
        }
    });
}
