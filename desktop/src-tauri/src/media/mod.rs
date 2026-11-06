//! Local media for the UI: thumbnails, full images and files, over loopback HTTP.
//!
//!   GET /thumb?path=…&token=…   JPEG thumbnail (cached on disk; videos get the Windows thumbnail)
//!   GET /image?path=…&token=…   the picture itself; formats a browser can't show (TIFF, HEIC…)
//!                               are decoded here and sent as PNG
//!   GET /file?path=…&token=…    the raw file, with Range support so video can seek
//!
//! Every request needs the token. The UI learns the port and token from the engine host's `info`.

mod thumbs;
#[cfg(windows)]
mod shell_thumb;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_http::{Header, Request, Response, Server, StatusCode};

#[derive(Clone)]
pub struct MediaInfo {
    pub port: u16,
    pub token: String,
}

/// Browsers show these themselves: /image sends them as they are.
const WEB_IMAGES: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "svg", "ico", "jfif"];
const WORKERS: usize = 8;

pub fn start() -> Result<MediaInfo, String> {
    let server = Arc::new(Server::http("127.0.0.1:0").map_err(|e| e.to_string())?);
    let port = server.server_addr().to_ip().map(|a| a.port()).ok_or("media server has no port")?;
    let token = crate::engine::new_token();
    let cache = Arc::new(thumbs::ThumbCache::open());
    cache.prune_in_background();

    for i in 0..WORKERS {
        let (server, token, cache) = (server.clone(), token.clone(), cache.clone());
        std::thread::Builder::new()
            .name(format!("media-{i}"))
            .spawn(move || {
                for req in server.incoming_requests() {
                    handle(req, &token, &cache);
                }
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(MediaInfo { port, token })
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("valid header")
}

fn fail(req: Request, code: u16, text: &str) {
    let resp = Response::from_string(text)
        .with_status_code(StatusCode(code))
        .with_header(header("Content-Type", "text/plain; charset=utf-8"))
        .with_header(header("Access-Control-Allow-Origin", "*"));
    let _ = req.respond(resp);
}

fn send_bytes(req: Request, body: Vec<u8>, ctype: &str) {
    let resp = Response::from_data(body)
        .with_header(header("Content-Type", ctype))
        .with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Cache-Control", "private, max-age=86400"));
    let _ = req.respond(resp);
}

fn handle(req: Request, token: &str, cache: &thumbs::ThumbCache) {
    // Cross-origin permission check a browser sends before some requests (e.g. "last N bytes" ranges)
    if req.method() == &tiny_http::Method::Options {
        let resp = Response::empty(StatusCode(204))
            .with_header(header("Access-Control-Allow-Origin", "*"))
            .with_header(header("Access-Control-Allow-Methods", "GET, OPTIONS"))
            .with_header(header("Access-Control-Allow-Headers", "Range"))
            .with_header(header("Access-Control-Max-Age", "86400"));
        let _ = req.respond(resp);
        return;
    }
    let Ok(url) = tauri::Url::parse(&format!("http://local{}", req.url())) else {
        return fail(req, 400, "bad request");
    };
    let q = |k: &str| url.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned()).unwrap_or_default();
    if q("token") != token {
        return fail(req, 403, "forbidden");
    }
    let path = PathBuf::from(q("path"));
    if path.as_os_str().is_empty() || !path.is_file() {
        return fail(req, 404, "not found");
    }
    match url.path() {
        "/thumb" => match cache.thumbnail(&path) {
            Some(jpeg) => send_bytes(req, jpeg, "image/jpeg"),
            None => fail(req, 415, "no thumbnail"),
        },
        "/image" => {
            if WEB_IMAGES.contains(&ext(&path).as_str()) {
                return send_file(req, &path);
            }
            match thumbs::full_image_png(&path) {
                Some(png) => send_bytes(req, png, "image/png"),
                None => fail(req, 415, "can't decode"),
            }
        }
        "/file" => send_file(req, &path),
        _ => fail(req, 404, "unknown route"),
    }
}

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn mime(path: &Path) -> &'static str {
    match ext(path).as_str() {
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "txt" | "log" => "text/plain; charset=utf-8",
        "json" => "application/json",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// "bytes=a-b", "bytes=a-" or "bytes=-n" → the inclusive range to send.
fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        return Some((size.saturating_sub(n), size.saturating_sub(1)));
    }
    let start: u64 = a.parse().ok()?;
    let end = if b.is_empty() { size.saturating_sub(1) } else { b.parse::<u64>().ok()?.min(size.saturating_sub(1)) };
    Some((start, end))
}

fn send_file(req: Request, path: &Path) {
    let Ok(mut file) = File::open(path) else { return fail(req, 404, "not found") };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let range = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .and_then(|h| parse_range(h.value.as_str(), size));
    let (start, end, partial) = match range {
        Some((s, e)) => (s, e, true),
        None => (0, size.saturating_sub(1), false),
    };
    if size == 0 || start > end {
        let resp = Response::empty(StatusCode(416)).with_header(header("Content-Range", &format!("bytes */{size}")));
        let _ = req.respond(resp);
        return;
    }
    let length = end - start + 1;
    if file.seek(SeekFrom::Start(start)).is_err() {
        return fail(req, 500, "can't read the file");
    }
    let mut resp = Response::new(
        StatusCode(if partial { 206 } else { 200 }),
        vec![
            header("Content-Type", mime(path)),
            header("Accept-Ranges", "bytes"),
            header("Access-Control-Allow-Origin", "*"),
            header("Access-Control-Expose-Headers", "Content-Range, Accept-Ranges"),
            header("Cache-Control", "private, max-age=3600"),
        ],
        file.take(length),
        Some(length as usize),
        None,
    );
    if partial {
        resp.add_header(header("Content-Range", &format!("bytes {start}-{end}/{size}")));
    }
    let _ = req.respond(resp);
}
