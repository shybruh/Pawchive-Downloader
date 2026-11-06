//! Signing in to a site inside the app.
//!
//! Browsers encrypt their cookies with keys only they can use (Chrome, Edge and Brave's
//! "app-bound" encryption), so importing a login from them rarely works. Instead the site's own
//! login page opens in a window of the app; when it closes, the shell reads that window's cookies
//! (HTTP-only ones included) and hands them to the UI, which saves them like a pasted cookie.
//!
//! The window has no access to the app's commands (capabilities only cover "main") and keeps its
//! own browsing data, apart from the app's interface.

use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const LABEL: &str = "site-login";
/// Only the archive sites the engine signs in to.
const SITES: &[&str] = &["pawchive.pw", "cum.st"];

/// The site the open sign-in window belongs to.
#[derive(Default)]
pub struct LoginSite(Mutex<String>);

#[derive(Clone, Serialize)]
struct LoginFinished {
    site: String,
    cookie: String,
    names: Vec<String>,
}

fn site_of(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    if url.scheme() != "https" {
        return None;
    }
    SITES
        .iter()
        .find(|d| host == **d || host.ends_with(&format!(".{d}")))
        .map(|d| d.to_string())
}

/// Opens `url` (a login page of one of SITES) in the sign-in window.
#[tauri::command]
pub async fn site_login_open(app: AppHandle, url: String) -> Result<(), String> {
    let url = Url::parse(&url).map_err(|e| e.to_string())?;
    let site = site_of(&url).ok_or("Only Pawchive and cum.st can be signed in to here")?;
    if let Some(old) = app.get_webview_window(LABEL) {
        let _ = old.destroy();
    }
    *app.state::<LoginSite>().0.lock().unwrap() = site.clone();
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("site-login");
    let win = WebviewWindowBuilder::new(&app, LABEL, WebviewUrl::External(url))
        .title(format!("Sign in to {site} — close this window when you're signed in"))
        .inner_size(560.0, 760.0)
        .min_inner_size(420.0, 520.0)
        .data_directory(data_dir)
        .build()
        .map_err(|e| e.to_string())?;

    let handle = app.clone();
    win.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            // Reading cookies on the window thread deadlocks WebView2, so it happens on a worker
            api.prevent_close();
            let handle = handle.clone();
            let site = site.clone();
            tauri::async_runtime::spawn(async move { finish(&handle, &site) });
        }
    });
    Ok(())
}

/// Closes the sign-in window and sends what it collected (the UI's "Done" button).
#[tauri::command]
pub async fn site_login_done(app: AppHandle) -> Result<(), String> {
    let site = app.state::<LoginSite>().0.lock().unwrap().clone();
    finish(&app, &site);
    Ok(())
}

/// Closes the window without saving anything.
#[tauri::command]
pub async fn site_login_cancel(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.destroy();
    }
    Ok(())
}

fn finish(app: &AppHandle, site: &str) {
    let Some(win) = app.get_webview_window(LABEL) else { return };
    let mut cookie = String::new();
    let mut names = Vec::new();
    if let Ok(url) = Url::parse(&format!("https://{site}/")) {
        for c in win.cookies_for_url(url).unwrap_or_default() {
            if !cookie.is_empty() {
                cookie.push_str("; ");
            }
            cookie.push_str(&format!("{}={}", c.name(), c.value()));
            names.push(c.name().to_string());
        }
    }
    let _ = win.destroy();
    let _ = app.emit_to("main", "site-login-finished", LoginFinished { site: site.to_string(), cookie, names });
}
