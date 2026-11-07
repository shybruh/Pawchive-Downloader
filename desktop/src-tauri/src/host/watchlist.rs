//! "appBridge.watchlistModel": the window's watchlist, from the Rust watchlist view
//! (watchlist/view.rs). The app keeps the view's artists up to date; toggling a favorite changes
//! the watchlist, so it is passed to the app.

use super::native::{Events, NativeObject, Prop};
use crate::downloader::service::ask_app;
use crate::watchlist::view::{self, View, ROLES};
use serde_json::{json, Map, Value};
use std::sync::MutexGuard;
use std::time::Duration;

pub const NAME: &str = "appBridge.watchlistModel";

fn view() -> MutexGuard<'static, View> {
    view::global().lock().unwrap()
}

pub struct WatchlistModel;

impl NativeObject for WatchlistModel {
    fn props(&self) -> Vec<Prop> {
        ["count", "totalCount", "updatedCount", "totalNewPosts", "autoCheckCount", "favoriteCount", "onDiskCount"]
            .into_iter()
            .map(|name| Prop { name, kind: "int", writable: false })
            .collect()
    }
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            ("setFilter", vec!["search_text", "service_filter"]),
            ("get", vec!["index"]),
            ("toggleFavorite", vec!["user_id", "service", "is_fav"]),
            ("refresh", vec![]),
        ]
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        view().props().get(prop).cloned().ok_or_else(|| format!("unknown property {prop:?}"))
    }
    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        let s = |i: usize| args.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        match method {
            "setFilter" => {
                let mut v = view();
                v.search = s(0);
                let f = s(1).to_lowercase();
                v.filter = if f.is_empty() { "all".into() } else { f };
                v.dirty = true;
                Ok(Value::Null)
            }
            "get" => {
                let v = view();
                let b = v.build();
                Ok(v.get(args.first().and_then(Value::as_u64).unwrap_or(u64::MAX) as usize, &b))
            }
            "refresh" => {
                let mut v = view();
                v.invalidate_disk();
                v.dirty = true;
                Ok(Value::Null)
            }
            "toggleFavorite" => ask_app("watchlist_call", json!([method, args])),
            m => Err(format!("{NAME} has no method {m:?}")),
        }
    }
    fn model_roles(&self) -> Option<&'static [&'static str]> {
        Some(ROLES)
    }
    fn rows(&self, start: usize, count: Option<usize>) -> Result<Value, String> {
        let v = view();
        let b = v.build();
        let total = b.shown.len();
        let s = start.min(total);
        let e = count.map_or(total, |c| (s + c).min(total));
        let rows: Vec<Value> = b.shown[s..e].iter().map(|&i| v.row(i, &b)).collect();
        Ok(json!({"total": total, "start": s, "rows": rows}))
    }
}

/// Tells the window what changed, a few times a second at most
pub fn start_pump(events: Events) {
    std::thread::Builder::new()
        .name("watchlist-view-events".into())
        .spawn(move || {
            let mut last = Map::new();
            loop {
                std::thread::sleep(Duration::from_millis(150));
                let props = {
                    let mut v = view();
                    if !v.dirty {
                        continue;
                    }
                    v.dirty = false;
                    v.props()
                };
                let changed: Map<String, Value> =
                    props.as_object().unwrap().iter().filter(|(k, val)| last.get(*k) != Some(val)).map(|(k, val)| (k.clone(), val.clone())).collect();
                if !changed.is_empty() {
                    events.props(NAME, Value::Object(changed));
                    last = props.as_object().unwrap().clone();
                }
                events.model(NAME, props["count"].as_u64().unwrap_or(0) as usize);
            }
        })
        .expect("watchlist view thread");
}
