//! The app bridge ("appBridge", bridge/app_bridge.py), moving to Rust a part at a time.
//!
//! The Python bridge still describes the object and answers most of it; the methods listed here
//! are answered in Rust (the host sends them here, see host/native.rs `Overlay`). Each part
//! is checked against the Python version before it moves.

pub mod decompressor;
pub mod gallery;
pub mod link_vault;
pub mod gallery_tools;

use crate::downloader::service;
use crate::host::native::{Events, Overlay};
use gallery::{Gallery, Outside};
use serde_json::{json, Value};
use std::sync::Arc;

/// What the Rust parts tell the window (as appBridge's signals) and the app (log, settings)
struct App {
    events: Events,
}

impl Outside for App {
    fn signal(&self, name: &str, args: Vec<Value>) {
        self.events.signal("appBridge", name, args);
    }
    fn log(&self, level: &str, message: &str, details: &str) {
        service::log_app(level, message, "gallery", details);
    }
    fn download_dir(&self) -> String {
        service::ask_app("download_dir", json!([])).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }
}

pub struct AppBridge {
    gallery: Arc<Gallery>,
}

impl AppBridge {
    pub fn new(events: Events) -> Arc<Self> {
        Arc::new(Self { gallery: Gallery::new(Arc::new(App { events })) })
    }

    /// The file explorer, shared with the Gallery's tools (galleryTools)
    pub fn gallery(&self) -> Arc<Gallery> {
        self.gallery.clone()
    }
}

const METHODS: &[&str] = &[
    "listDirectory", "getLastListingTotal", "getBreadcrumbs", "getSystemDrives", "openPathInSystem", "moveItems", "renameItem",
    "scanDuplicates", "scanDuplicatesAsync", "getPathSafetyInfo", "revealFileInExplorer",
    "deleteItems", "getDownloadDir", "openFolder", "showDownloadsInGallery",
    // (the watchlist: watchlist/entries.rs)
    "removeFromWatchlist", "setWatchlistAutoCheck", "setAllWatchlistAutoCheck", "disableAllWatchlistAutoCheck", "ignoreWatchlistPost",
    // (the queue's retries: downloader/service.rs queue_window)
    "retryFailed", "retrySingleTask", "clearFailedTasks",
];

/// The watchlist's buttons: answered by the Rust watchlist, else by the Python bridge (the engine
/// hasn't been given the watchlist yet, or the Rust one is turned off)
fn watchlist_button(method: &str, args: Vec<Value>) -> Result<Value, String> {
    match crate::watchlist::entries::window_call(method, &args) {
        Some(r) => r,
        None => service::ask_app("watchlist_button", json!([method, args])),
    }
}

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).and_then(Value::as_str).unwrap_or("").to_string()
}

fn arg_bool(args: &[Value], i: usize) -> bool {
    args.get(i).and_then(Value::as_bool).unwrap_or(false)
}

fn arg_list(args: &[Value], i: usize) -> Vec<String> {
    args.get(i).and_then(Value::as_array).into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect()
}

impl Overlay for AppBridge {
    fn handles(&self, method: &str) -> bool {
        METHODS.contains(&method)
    }

    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        let g = &self.gallery;
        if matches!(method, "removeFromWatchlist" | "setWatchlistAutoCheck" | "setAllWatchlistAutoCheck" | "disableAllWatchlistAutoCheck" | "ignoreWatchlistPost") {
            return watchlist_button(method, args);
        }
        if matches!(method, "retryFailed" | "retrySingleTask" | "clearFailedTasks") {
            // Done on the engine's queue, else by the Python bridge (cloud links, saved failed files)
            return match service::queue_window(method, &args) {
                Some(v) => Ok(v),
                None => service::ask_app("app_button", json!([method, args])),
            };
        }
        Ok(match method {
            "listDirectory" => {
                let max = args.get(1).and_then(Value::as_u64).unwrap_or(1500) as usize;
                json!(g.list_directory(&arg_str(&args, 0), max))
            }
            "getLastListingTotal" => json!(g.last_listing_total()),
            "getBreadcrumbs" => json!(g.breadcrumbs(&arg_str(&args, 0))),
            "getSystemDrives" => json!(g.system_drives()),
            "openPathInSystem" => {
                g.open_path_in_system(&arg_str(&args, 0));
                Value::Null
            }
            "moveItems" => g.move_items(&arg_list(&args, 0), &arg_str(&args, 1)),
            "renameItem" => g.rename_item(&arg_str(&args, 0), &arg_str(&args, 1)),
            "scanDuplicates" => json!(g.scan_duplicates(&arg_str(&args, 0), arg_bool(&args, 1))),
            "scanDuplicatesAsync" => {
                g.scan_duplicates_async(&arg_str(&args, 0), &arg_str(&args, 1), arg_bool(&args, 2));
                Value::Null
            }
            "getPathSafetyInfo" => g.path_safety(&arg_str(&args, 0)),
            "revealFileInExplorer" => json!(g.reveal(&arg_str(&args, 0))),
            "deleteItems" => g.delete_items(&arg_list(&args, 0)),
            "getDownloadDir" => json!(g.download_dir_or_home()),
            "openFolder" => {
                g.open_folder(&arg_str(&args, 0));
                Value::Null
            }
            "showDownloadsInGallery" => {
                // (the queue the window shows: the engine's, or the app's with the old downloader)
                let targets: Vec<String> = crate::downloader::queue_view::global().lock().unwrap().rows().iter()
                    .map(|r| r.target_path.clone()).collect();
                g.show_downloads(&targets);
                Value::Null
            }
            m => return Err(format!("appBridge has no Rust method {m:?}")),
        })
    }
}
