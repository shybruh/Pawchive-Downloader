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

/// Runs before the site's own scripts: ad scripts that open pop-unders get nothing, and links
/// that would open a new tab open in place instead (where the navigation check applies).
const NO_POPUPS: &str = r#"
(() => {
  window.open = () => null;
  document.addEventListener("click", (e) => {
    const a = e.target instanceof Element ? e.target.closest("a[target]") : null;
    if (a) a.removeAttribute("target");
  }, true);
})();
"#;

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
    // The sites run pop-up and redirect ads: the window only ever shows pages of the site itself,
    // and nothing on it can open another window
    let allowed = site.clone();
    let win = WebviewWindowBuilder::new(&app, LABEL, WebviewUrl::External(url))
        .title(format!("Sign in to {site} — close this window when you're signed in"))
        .inner_size(560.0, 760.0)
        .min_inner_size(420.0, 520.0)
        .data_directory(data_dir)
        .on_navigation(move |to| to.scheme() == "about" || site_of(to).as_deref() == Some(allowed.as_str()))
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .initialization_script(NO_POPUPS)
        .build()
        .map_err(|e| e.to_string())?;

    #[cfg(windows)]
    block_ads(&win, &site);

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

/// Hosts a login page may load besides the site itself: Cloudflare's "are you human" check and
/// the common captcha services.
const HELPERS: &[&str] = &[
    "challenges.cloudflare.com",
    "hcaptcha.com",
    "recaptcha.net",
    "www.google.com",
    "www.gstatic.com",
];

fn host_allowed(uri: &str, site: &str) -> bool {
    let Ok(url) = Url::parse(uri) else { return true };
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return true; // data:, blob: and the like come from the page itself
    }
    let Some(host) = url.host_str() else { return false };
    let under = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    under(site) || HELPERS.iter().any(|d| under(d))
}

/// Ads on the sites come from ad networks on other domains: every request the window makes to a
/// host that isn't the site (or a captcha helper) gets an empty "blocked" answer instead.
#[cfg(windows)]
fn block_ads(win: &tauri::WebviewWindow, site: &str) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_2, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
    };
    use webview2_com::{take_pwstr, WebResourceRequestedEventHandler};
    use windows_core::{Interface, HSTRING, PWSTR};

    let site = site.to_string();
    let _ = win.with_webview(move |pw| unsafe {
        let Ok(core) = pw.controller().CoreWebView2() else { return };
        let Ok(env) = core.cast::<ICoreWebView2_2>().and_then(|c| c.Environment()) else { return };
        if core.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL).is_err() {
            return;
        }
        let mut token = 0i64;
        let _ = core.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut uri = PWSTR::null();
                args.Request()?.Uri(&mut uri)?;
                let uri = take_pwstr(uri);
                if !host_allowed(&uri, &site) {
                    let blocked = env.CreateWebResourceResponse(None, 403, &HSTRING::from("Blocked"), &HSTRING::new())?;
                    args.SetResponse(&blocked)?;
                }
                Ok(())
            })),
            &mut token,
        );
    });
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
