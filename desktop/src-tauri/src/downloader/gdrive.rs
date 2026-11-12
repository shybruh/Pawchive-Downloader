//! Google Drive links: single files (and Google Docs / Sheets / Slides, exported) and folders. A
//! port of services/cloud_downloader.py download_gdrive_link with the part of gdown (6.5) it used:
//! the file's name first, Drive's "download anyway" step for big files, its quota and access
//! errors, a ".part" file continued on the next try; folders read from Drive's embedded folder
//! view. The same log lines and error texts (the app picks its messages from them).
//!
//! Checked against the Python download on made-up Drive pages (tests/fixtures/gdrive.json,
//! tools/golden_gdrive.py).

use super::albums::{parse_qs, quote_plus, split_url};
use super::cloud::{safe_name, skip_reason, Job, Resp};
use regex::Regex;
use scraper::{Html, Selector};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

macro_rules! re {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new($p).expect("valid pattern"))
    }};
}

const FILE_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_10_1) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/39.0.2171.95 Safari/537.36";
const FOLDER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/98.0.4758.102 Safari/537.36";
const UC: &str = "https://drive.google.com/uc?id=";
const FAQ: &str = "Check FAQ in https://github.com/wkentaro/gdown?tab=readme-ov-file#faq.";
const QUOTA_MSG: &str = "   [Google Drive] ⚠️ Bandwidth Quota Exceeded: Google has temporarily locked public downloads for this file ('Too many users have viewed or downloaded this file recently'). Google imposes a 24-hour cooldown on heavily accessed files. Please try again later or open in a browser while logged in.";

/// gdown's errors, by kind (a "download error" in a folder only fails that file)
#[derive(Debug)]
enum Err {
    /// FileURLRetrievalError / DownloadError
    Download(String),
    /// Anything else (an HTTP error, no connection): stops a folder too
    Other(String),
}

impl Err {
    fn text(&self) -> &str {
        match self {
            Err::Download(s) | Err::Other(s) => s,
        }
    }
}

// ── Names and links ───────────────────────────────────────────────────────────────────────────

/// gdown's _sanitize_filename (Windows rules on Windows)
pub fn sanitize(name: &str) -> String {
    let mut f = name.replace('\0', "").replace(['/', '\\'], "_").trim().to_string();
    if cfg!(windows) {
        f = re!(r#"[<>:"|?*\x01-\x1f]"#).replace_all(&f, "_").trim_end_matches(['.', ' ']).to_string();
        let stem = f.split('.').next().unwrap_or("").trim_end_matches(' ').to_uppercase();
        if re!(r"^(?:CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|(?:COM|LPT)[1-9\x{b9}\x{b2}\x{b3}])$").is_match(&stem) {
            f = format!("_{f}");
        }
    }
    if f.is_empty() || f == "." || f == ".." { "_".into() } else { f }
}

fn host(url: &str) -> String {
    let rest = url.split_once("://").map(|x| x.1).unwrap_or("");
    let h = rest.split(['/', '?', '#']).next().unwrap_or("");
    let h = h.rsplit('@').next().unwrap_or(h);
    h.split(':').next().unwrap_or("").to_lowercase()
}

fn is_drive(url: &str) -> bool {
    matches!(host(url).as_str(), "drive.google.com" | "docs.google.com")
}

/// gdown's parse_url: (file id, whether it's a download link)
fn parse_url(url: &str) -> (Option<String>, bool) {
    let (_, path, query, _) = split_url(url);
    let dl = path.ends_with("/uc");
    if !is_drive(url) {
        return (None, dl);
    }
    let q = parse_qs(&query);
    if let Some((_, ids)) = q.iter().find(|(k, _)| k == "id") {
        return (if ids.len() == 1 { Some(ids[0].clone()) } else { None }, dl);
    }
    for p in [
        r"^/file/d/([-\w]+)(?:/(?:edit|view|preview))?/?$",
        r"^/file/u/[0-9]+/d/([-\w]+)(?:/(?:edit|view|preview))?/?$",
        r"^/document/d/(.*?)/(edit|htmlview|view)$",
        r"^/document/u/[0-9]+/d/(.*?)/(edit|htmlview|view)$",
        r"^/presentation/d/(.*?)/(edit|htmlview|view)$",
        r"^/presentation/u/[0-9]+/d/(.*?)/(edit|htmlview|view)$",
        r"^/spreadsheets/d/(.*?)/(edit|htmlview|view)$",
        r"^/spreadsheets/u/[0-9]+/d/(.*?)/(edit|htmlview|view)$",
    ] {
        if let Some(c) = Regex::new(p).ok().and_then(|r| r.captures(&path)) {
            return (Some(c[1].to_string()), dl);
        }
    }
    (None, dl)
}

fn folder_id_of(url: &str) -> Option<String> {
    if !is_drive(url) {
        return None;
    }
    re!(r"^/drive/folders/([-\w]{25,})(?:/|$)").captures(&split_url(url).1).map(|c| c[1].to_string())
}

/// The file name a download answer names (Content-Disposition)
fn filename_of(r: &Resp) -> Option<String> {
    let cd = super::albums::unquote(r.header("content-disposition"));
    if let Some(c) = re!(r"filename\*=UTF-8''(.*)").captures(&cd) {
        return Some(sanitize(&c[1]));
    }
    re!(r#"attachment; filename="(.*?)""#).captures(&cd).map(|c| sanitize(&c[1]))
}

fn identity_only(r: &Resp, header: &str) -> bool {
    r.header(header).split(',').all(|e| matches!(e.trim().to_lowercase().as_str(), "" | "identity"))
}

fn comparable(r: &Resp) -> bool {
    identity_only(r, "content-encoding") && identity_only(r, "transfer-encoding")
}

fn content_length(r: &Resp) -> Option<u64> {
    if !comparable(r) {
        return None;
    }
    r.headers.get("content-length")?.trim().parse::<i64>().ok().filter(|n| *n >= 0).map(|n| n as u64)
}

/// Python's textwrap.wrap(text, 70): words (split after hyphens between letters) filled greedily,
/// words longer than a line cut
fn wrap(text: &str) -> Vec<String> {
    const W: usize = 70;
    let letter = |c: char| c.is_alphabetic() || c == '_';
    let text: Vec<char> = text.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).collect();
    // Chunks: runs of spaces, and words cut after a hyphen between letters ("well-known")
    let mut chunks: Vec<Vec<char>> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let space = text[i] == ' ';
        let mut j = i;
        while j < text.len() && (text[j] == ' ') == space {
            j += 1;
        }
        if space {
            chunks.push(text[i..j].to_vec());
        } else {
            let w = &text[i..j];
            let mut start = 0;
            for k in 0..w.len() {
                if w[k] != '-' || k == start {
                    continue;
                }
                let before = (k >= 2 && letter(w[k - 1]) && letter(w[k - 2])) || (k >= 3 && letter(w[k - 1]) && w[k - 2] == '-' && letter(w[k - 3]));
                let after = k + 1 < w.len() && letter(w[k + 1])
                    && ((k + 2 < w.len() && letter(w[k + 2])) || (k + 3 < w.len() && w[k + 2] == '-' && letter(w[k + 3])));
                if before && after {
                    chunks.push(w[start..=k].to_vec());
                    start = k + 1;
                }
            }
            chunks.push(w[start..].to_vec());
        }
        i = j;
    }
    chunks.reverse();
    let mut lines = Vec::new();
    while !chunks.is_empty() {
        let mut line: Vec<Vec<char>> = Vec::new();
        let mut len = 0;
        if !lines.is_empty() && chunks.last().is_some_and(|c| c.iter().all(|x| *x == ' ')) {
            chunks.pop();
        }
        while let Some(c) = chunks.last() {
            if len + c.len() <= W {
                len += c.len();
                line.push(chunks.pop().unwrap());
            } else {
                break;
            }
        }
        if let Some(c) = chunks.last().filter(|c| c.len() > W) {
            // (a word longer than a line: as much as fits, at a hyphen when there's one)
            let space_left = (W - len).max(1);
            let mut end = space_left;
            if let Some(h) = c[..space_left.min(c.len())].iter().rposition(|x| *x == '-') {
                if h > 0 && c[..h].iter().any(|x| *x != '-') {
                    end = h + 1;
                }
            }
            let c = chunks.pop().unwrap();
            line.push(c[..end].to_vec());
            chunks.push(c[end..].to_vec());
        }
        if line.last().is_some_and(|c| c.iter().all(|x| *x == ' ')) {
            line.pop();
        }
        if !line.is_empty() {
            lines.push(line.concat().into_iter().collect::<String>());
        }
    }
    lines
}

/// Drive's page between a link and its file: where the file really is
fn confirmation(contents: &str) -> Result<String, String> {
    for line in contents.lines() {
        if let Some(c) = re!(r#"href="(/uc\?export=download[^"]+)"#).captures(line) {
            return Ok(format!("https://docs.google.com{}", &c[1]).replace("&amp;", "&"));
        }
        if line.contains("download-form") {
            let doc = Html::parse_fragment(line);
            let form_sel = Selector::parse("#download-form").unwrap();
            if let Some(form) = doc.select(&form_sel).next() {
                let action = form.value().attr("action").unwrap_or("").replace("&amp;", "&");
                let (origin, path, query, frag) = split_url(&action);
                let mut q = parse_qs(&query);
                let input = Selector::parse(r#"input[type="hidden"]"#).unwrap();
                for i in form.select(&input) {
                    let (n, v) = (i.value().attr("name").unwrap_or("").to_string(), i.value().attr("value").unwrap_or("").to_string());
                    match q.iter_mut().find(|(k, _)| *k == n) {
                        Some(e) => e.1 = vec![v],
                        None => q.push((n, vec![v])),
                    }
                }
                let qs: Vec<String> = q.iter().flat_map(|(k, vs)| vs.iter().map(move |v| format!("{}={}", quote_plus(k), quote_plus(v)))).collect();
                let mut u = format!("{origin}{path}");
                if !qs.is_empty() {
                    u = format!("{u}?{}", qs.join("&"));
                }
                if !frag.is_empty() {
                    u = format!("{u}#{frag}");
                }
                return Ok(u);
            }
        }
        if let Some(c) = re!(r#""downloadUrl":"([^"]+)"#).captures(line) {
            return Ok(c[1].replace("\\u003d", "=").replace("\\u0026", "&"));
        }
        if let Some(c) = re!(r#"<p class="uc-error-subcaption">(.*)</p>"#).captures(line) {
            return Err(c[1].to_string());
        }
    }
    Err(format!(
        "Cannot retrieve the public link of the file. You may need to change the permission to 'Anyone with the link', or have had many accesses. {FAQ}"
    ))
}

// ── gdown's download ──────────────────────────────────────────────────────────────────────────

fn get(job: &Job, url: &str, ua: &str, extra: &[(&str, String)]) -> Result<Resp, Err> {
    let mut h = vec![("User-Agent", ua.to_string())];
    h.extend(extra.iter().cloned());
    job.web.get(url, &h).map_err(Err::Other)
}

/// The answer holding the file: through Docs' export and Drive's confirmation pages
fn file_response(job: &Job, mut url: String, file_id: Option<&str>, extra: &[(&str, String)]) -> Result<(Resp, String), Err> {
    let origin = url.clone();
    for _ in 0..20 {
        let r = get(job, &url, FILE_UA, extra)?;
        let Some(id) = file_id else { return Ok((r, url)) };
        if url == origin && r.status == 500 {
            url = format!("https://drive.google.com/open?id={id}");
            continue;
        }
        let ct = r.headers.get("content-type").ok_or_else(|| Err::Other("'Content-Type'".into()))?;
        if ct.starts_with("text/html") {
            let export = [("/document/", "document", "docx"), ("/spreadsheets/", "spreadsheets", "xlsx"), ("/presentation/", "presentation", "pptx")]
                .into_iter()
                .find(|(p, _, _)| r.url.contains(p) && !r.url.contains("/export"));
            if let Some((_, kind, fmt)) = export {
                url = format!("https://docs.google.com/{kind}/d/{id}/export?format={fmt}");
                continue;
            }
        }
        if r.headers.contains_key("content-disposition") {
            return Ok((r, url));
        }
        match confirmation(&r.text()) {
            Ok(u) => url = u,
            Err(e) => {
                let inner: Vec<String> = wrap(&e).into_iter().map(|l| format!("\t{l}")).collect();
                return Err(Err::Download(format!(
                    "Failed to retrieve file url:\n\n{}\n\nYou may still be able to access the file from the browser:\n\n\t{origin}\n\nbut Gdown can't. Please check connections and permissions.",
                    inner.join("\n")
                )));
            }
        }
    }
    Err(Err::Other("too many redirects".into()))
}

fn rand_suffix() -> String {
    const CH: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| CH[*x as usize % CH.len()] as char).collect()
}

/// The ".part" file for `output`: an earlier one when continuing, else a new one
fn partial(output: &Path, resume: bool) -> Result<(PathBuf, bool), Err> {
    let dir = output.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let base = output.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut existing: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| Err::Other(e.to_string()))?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            (n.starts_with(&base) && n.ends_with(".part") && n.chars().count() == base.chars().count() + 13).then(|| e.path())
        })
        .collect();
    existing.sort();
    if resume && !existing.is_empty() {
        if existing.len() != 1 {
            let mut lines = vec!["There are multiple temporary files to resume:".to_string(), String::new()];
            lines.extend(existing.iter().map(|f| format!("\t{}", f.to_string_lossy())));
            lines.push(String::new());
            lines.push("Please remove them except one to resume downloading.".into());
            return Err(Err::Download(lines.join("\n")));
        }
        return Ok((existing.remove(0), true));
    }
    for _ in 0..100 {
        let p = dir.join(format!("{base}{}.part", rand_suffix()));
        if std::fs::OpenOptions::new().write(true).create_new(true).open(&p).is_ok() {
            return Ok((p, false));
        }
    }
    Err(Err::Other("couldn't make a temporary file".into()))
}

/// Writes the file's bytes to `out` (continuing at `start`): how many were added
fn write_body(job: &Job, mut r: Resp, url: &str, out: &mut std::fs::File, tmp: &Path, start: u64, name: &str) -> Result<(), Err> {
    let mut expected_total = content_length(&r);
    let validator = r.headers.get("etag").filter(|v| !v.starts_with("W/")).or_else(|| r.headers.get("last-modified")).cloned();
    let mut range_size = None;
    if start != 0 {
        let mut h = vec![("Range", format!("bytes={start}-"))];
        if let Some(v) = validator.filter(|v| !v.is_empty()) {
            h.push(("If-Range", v));
        }
        r = get(job, url, FILE_UA, &[&h[..], &[("Accept-Encoding", "identity".to_string())]].concat())?;
        let m = re!(r"^bytes (\d+)-(\d+)/(\d+|\*)$").captures(r.header("content-range")).map(|c| (c[1].to_string(), c[2].to_string(), c[3].to_string()));
        let ok = m.as_ref().is_some_and(|(a, b, t)| {
            let (a, b): (u64, u64) = (a.parse().unwrap_or(u64::MAX), b.parse().unwrap_or(0));
            r.status == 206 && a == start && b >= start && (t == "*" || t.parse::<u64>().ok() == Some(b + 1))
                && expected_total.is_none_or(|e| b + 1 == e)
                && identity_only(&r, "content-encoding")
                && (!comparable(&r) || content_length(&r).is_none_or(|l| l == b - start + 1))
        });
        if !ok {
            return Err(Err::Download("Server did not honor the requested byte range; the partial file is preserved.".into()));
        }
        let (_, b, t) = m.unwrap();
        let b: u64 = b.parse().unwrap_or(0);
        range_size = Some(if t == "*" { b - start + 1 } else { t.parse::<u64>().unwrap_or(0) - start });
    }
    let length = range_size.or_else(|| content_length(&r));
    let total = length.map(|l| l + start);
    let expected = if range_size.is_some() || comparable(&r) { length } else { None };
    if expected.is_some() {
        expected_total = total;
    }
    let _ = expected_total;
    let mut buf = vec![0u8; 512 * 1024];
    let mut received = 0u64;
    let mut last = Instant::now();
    loop {
        if job.cancelled() {
            return Err(Err::Other("Cancelled by user".into()));
        }
        while super::cloud::PAUSED.load(std::sync::atomic::Ordering::Relaxed) && !job.cancelled() {
            std::thread::sleep(Duration::from_millis(500));
        }
        let k = match std::io::Read::read(&mut r.body, &mut buf) {
            Ok(0) => break,
            Ok(k) => k,
            Err(e) => {
                if expected.is_some() {
                    break; // (counted as an incomplete download below)
                }
                return Err(Err::Other(e.to_string()));
            }
        };
        if expected.is_some_and(|e| received + k as u64 > e) {
            return Err(Err::Download("Response exceeds the announced byte range".into()));
        }
        crate::transfer::limiter::take(k as u64, job.cancel);
        out.write_all(&buf[..k]).map_err(|e| Err::Other(e.to_string()))?;
        received += k as u64;
        if last.elapsed() >= Duration::from_millis(250) {
            (job.progress)(name, start + received, total.unwrap_or(0), 1, 1);
            last = Instant::now();
        }
    }
    if let Some(e) = expected.filter(|e| received < *e) {
        let _ = e;
        return Err(Err::Download(format!(
            "Download is incomplete: received {} bytes but the server announced {} bytes.\nThe received bytes are kept in {}, which resume (--continue on the command line) picks up.",
            start + received,
            total.unwrap_or(0),
            tmp.to_string_lossy()
        )));
    }
    Ok(())
}

enum Output {
    /// A folder: the file's own name in it
    Dir(PathBuf),
    File(PathBuf),
}

/// gdown.download: the file (by Drive id or link) saved to `output`; with `skip`, only its name.
/// Answers the saved path (or the name).
fn download(job: &Job, id: Option<&str>, url: Option<&str>, output: Output, skip: bool, resume: bool) -> Result<String, Err> {
    let mut url = match id {
        Some(i) => format!("{UC}{i}"),
        None => url.unwrap_or("").to_string(),
    };
    let mut origin = url.clone();
    let (file_id, mut is_dl) = parse_url(&url);
    if let Some(f) = &file_id {
        url = format!("{UC}{f}");
        origin = url.clone();
        is_dl = true;
    }
    let extra: Vec<(&str, String)> = if !skip && resume { vec![("Accept-Encoding", "identity".into())] } else { Vec::new() };
    let (r, url) = file_response(job, url, file_id.as_deref(), &extra)?;
    let name = if file_id.is_some() && is_dl { filename_of(&r) } else { None };
    if skip {
        return name.ok_or_else(|| {
            Err::Download(format!("Could not determine the Google Drive filename; --json requires a resolvable Google Drive file (got: {origin})"))
        });
    }
    if r.status >= 400 {
        return Err(Err::Other(format!("HTTP {}", r.status)));
    }
    let name = name.unwrap_or_else(|| sanitize(super::py::basename(&url)));
    let out = match output {
        Output::Dir(d) => {
            let _ = std::fs::create_dir_all(&d);
            d.join(&name)
        }
        Output::File(f) if f.is_dir() => f.join(&name),
        Output::File(f) => f,
    };
    if resume && out.is_file() {
        return Ok(out.to_string_lossy().to_string());
    }
    let (tmp, _) = partial(&out, resume)?;
    let last_modified = r.headers.get("last-modified").cloned();
    let mut f = std::fs::OpenOptions::new().append(true).open(&tmp).map_err(|e| Err::Other(e.to_string()))?;
    let start = f.metadata().map(|m| m.len()).unwrap_or(0);
    write_body(job, r, &url, &mut f, &tmp, start, &name)?;
    drop(f);
    if job.cancelled() {
        return Err(Err::Other("Cancelled by user".into()));
    }
    let _ = std::fs::remove_file(&out);
    std::fs::rename(&tmp, &out).map_err(|e| Err::Other(e.to_string()))?;
    if let Some(t) = last_modified.and_then(|lm| chrono::DateTime::parse_from_rfc2822(&lm).ok()) {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&out) {
            let _ = f.set_modified(std::time::SystemTime::from(t));
        }
    }
    Ok(out.to_string_lossy().to_string())
}

// ── gdown's folders ───────────────────────────────────────────────────────────────────────────

const FOLDER: &str = "application/vnd.google-apps.folder";

struct Item {
    id: String,
    name: String,
    kind: String,
    children: Vec<Item>,
}

fn list_folder(job: &Job, id: &str, depth: usize) -> Result<Item, Err> {
    let r = get(job, &format!("https://drive.google.com/embeddedfolderview?id={}", quote_plus(id)), FOLDER_UA, &[])?;
    if r.status != 200 {
        return Err(Err::Download(format!(
            "Failed to retrieve folder contents for folder ID: {id} (status code {}). You may need to change the permission to 'Anyone with the link', or have had many accesses. {FAQ}",
            r.status
        )));
    }
    let doc = Html::parse_document(&r.text());
    let title = doc.select(&Selector::parse("title").unwrap()).next().and_then(|t| {
        let kids: Vec<_> = t.children().collect();
        (kids.len() == 1).then(|| kids[0].value().as_text().map(|s| s.to_string())).flatten()
    });
    let Some(title) = title else {
        return Err(Err::Download(format!("Failed to parse folder contents for folder ID: {id}. The page structure may have changed.")));
    };
    let mut item = Item { id: id.into(), name: title, kind: FOLDER.into(), children: Vec::new() };
    for a in doc.select(&Selector::parse("a").unwrap()) {
        let href = a.value().attr("href").unwrap_or("");
        let name: String = a.text().map(str::trim).collect();
        if let Some(c) = re!(r"^https://drive\.google\.com/file/d/([-\w]{25,})/view").captures(href) {
            item.children.push(Item { id: c[1].into(), name, kind: "application/octet-stream".into(), children: Vec::new() });
        } else if let Some(c) = re!(r"^https://docs\.google\.com/(\w+)/d/([-\w]{25,})/").captures(href) {
            let kind = c[1].strip_suffix('s').unwrap_or(&c[1]).to_string();
            item.children.push(Item { id: c[2].into(), name, kind: format!("application/vnd.google-apps.{kind}"), children: Vec::new() });
        } else if let Some(fid) = folder_id_of(href) {
            if depth > 32 {
                continue;
            }
            item.children.push(list_folder(job, &fid, depth + 1)?);
        }
    }
    Ok(item)
}

fn layout(item: &mut Item, prev: &Path, out: &mut Vec<(Option<(String, String)>, PathBuf)>) {
    for c in item.children.iter_mut() {
        c.name = sanitize(&c.name);
        if c.kind == FOLDER {
            let p = prev.join(&c.name);
            out.push((None, p.clone()));
            layout(c, &p, out);
        } else if c.children.is_empty() {
            out.push((Some((c.id.clone(), c.kind.clone())), prev.join(&c.name)));
        }
    }
}

/// gdown.download_folder(id, output): every file saved under `root`; the files saved
fn download_folder(job: &Job, id: &str, root: &Path) -> Result<Vec<PathBuf>, Err> {
    let mut tree = list_folder(job, id, 0)?;
    let mut plan = Vec::new();
    layout(&mut tree, Path::new(""), &mut plan);
    let _ = std::fs::create_dir_all(root);
    let mut files = Vec::new();
    let mut failed: Vec<PathBuf> = Vec::new();
    for (f, rel) in plan {
        let local = root.join(&rel);
        let Some((fid, kind)) = f else {
            let _ = std::fs::create_dir_all(&local);
            continue;
        };
        if job.cancelled() {
            return Err(Err::Other("Cancelled by user".into()));
        }
        let native = kind.starts_with("application/vnd.google-apps.") && kind != FOLDER;
        let output = if native { Output::Dir(local.parent().map(Path::to_path_buf).unwrap_or_default()) } else { Output::File(local.clone()) };
        match download(job, None, Some(&format!("{UC}{fid}")), output, false, false) {
            Ok(p) => files.push(PathBuf::from(p)),
            Err(Err::Download(_)) => failed.push(local),
            Err(e) => return Err(e),
        }
    }
    if !failed.is_empty() {
        let list: Vec<String> = failed.iter().map(|p| format!("- {}", p.to_string_lossy())).collect();
        return Err(Err::Download(format!("Failed to download the following files:\n{}", list.join("\n"))));
    }
    Ok(files)
}

// ── The app's Google Drive download (download_gdrive_link) ────────────────────────────────────

fn quota_page(p: &Path) -> bool {
    if std::fs::metadata(p).map(|m| m.len() > 65536).unwrap_or(true) {
        return false;
    }
    let mut head = vec![0u8; 2048];
    let n = std::fs::File::open(p).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).unwrap_or(0);
    let h = &head[..n];
    let has = |s: &[u8]| h.windows(s.len()).any(|w| w == s);
    has(b"<title>Google Drive - Quota exceeded</title>") || has(b"uc-error-subcaption") || has(b"Too many users have viewed")
}

/// A Drive file's name from a quick request (when gdown couldn't tell it)
fn resolve_name(job: &Job, id: &str, fallback: &str) -> String {
    if let Ok(r) = job.web.get(&format!("https://drive.google.com/uc?export=download&id={id}"), &[]) {
        if let Some(c) = re!(r#"(?i)filename\*?=(?:UTF-8''')?"?([^"\n;]+)"#).captures(r.header("content-disposition")) {
            return c[1].trim().trim_matches('"').trim_matches('\'').to_string();
        }
    }
    fallback.into()
}

pub fn gdrive(job: &Job, url: &str, target: &Path) -> bool {
    let _ = std::fs::create_dir_all(target);
    job.say(&format!("   [Google Drive] Initializing download for: {url}"));
    let (_, path, query, _) = split_url(url);
    let qs = parse_qs(&query);
    let qid = qs.iter().find(|(k, _)| k == "id").map(|(_, v)| v[0].clone());
    let (folder, id): (bool, Option<String>) = if let Some(c) = re!(r"/folders/([a-zA-Z0-9_-]+)").captures(&path) {
        (true, Some(c[1].to_string()))
    } else if path.contains("folderview") || query.contains("folders") {
        (true, qid)
    } else if let Some(c) = re!(r"/file/(?:u/\d+/)?d/([a-zA-Z0-9_-]+)").captures(&path) {
        (false, Some(c[1].to_string()))
    } else if let Some(c) = re!(r"/(?:document|presentation|spreadsheets)/d/([a-zA-Z0-9_-]+)").captures(&path) {
        (false, Some(c[1].to_string()))
    } else if let Some(i) = qid {
        let folder = path.contains("open")
            && job.web.get_no_follow(&format!("https://drive.google.com/open?id={i}")).is_ok_and(|r| {
                let loc = r.header("location");
                loc.contains("/folders/") || loc.contains("folderview")
            });
        (folder, Some(i))
    } else {
        (false, None)
    };

    if let (true, Some(id)) = (folder, id.as_ref()) {
        job.say(&format!("   [Google Drive] 📁 Detected folder ID: {id}. Downloading folder..."));
        return match download_folder(job, id, target) {
            Ok(list) if !list.is_empty() => {
                let mut good = 0;
                for p in list.iter().filter(|p| p.is_file()) {
                    if quota_page(p) {
                        let _ = std::fs::remove_file(p);
                        job.say("   [Google Drive] ⚠️ Quota exceeded for one file in folder.");
                        continue;
                    }
                    let rel = p.strip_prefix(target).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
                    let size = std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0);
                    match skip_reason(&rel, &job.skip_words, Some(size), job.options.as_ref()) {
                        Some(why) => {
                            let _ = std::fs::remove_file(p);
                            job.say(&format!("   [Google Drive] ⏭️ Skipped '{rel}': {why}"));
                        }
                        None => good += 1,
                    }
                }
                job.say(&format!("   [Google Drive] ✅ Folder download finished: {good} file(s) saved to {}", target.to_string_lossy()));
                good > 0
            }
            Ok(_) => {
                job.say("   [Google Drive] ⚠️ Folder appears empty or has restricted permissions.");
                false
            }
            Err(e) => {
                let s = e.text();
                if s.contains("Too many users") || s.contains("Quota") {
                    job.say("   [Google Drive] ⚠️ Bandwidth Quota Exceeded on folder. Please try again later.");
                } else if s.contains("Cancelled") {
                    job.say("   [Google Drive] ℹ️ Download cancelled.");
                } else {
                    job.say(&format!("   [Google Drive] ❌ Folder download error: {s}"));
                }
                false
            }
        };
    }

    // A file: its name first, then the file
    let sep = std::path::MAIN_SEPARATOR;
    let dir = Output::Dir(PathBuf::from(format!("{}{sep}", target.to_string_lossy())));
    let resolved = download(job, id.as_deref(), if id.is_none() { Some(url) } else { None }, dir, true, false)
        .ok()
        .map(|p| super::py::basename(&p).to_string())
        .filter(|n| !n.is_empty());
    let resolved = resolved.unwrap_or_else(|| match &id {
        Some(i) => resolve_name(job, i, &format!("gdrive_{i}")),
        None => {
            let b = super::py::basename(&path).to_string();
            if b.is_empty() { "gdrive_file".into() } else { b }
        }
    });
    let resolved = safe_name(Some(&resolved), &format!("gdrive_{}", id.as_deref().unwrap_or("file")));
    let output = target.join(&resolved);
    if let Some(why) = skip_reason(&resolved, &job.skip_words, None, job.options.as_ref()) {
        job.say(&format!("   [Google Drive] ⏭️ Skipping '{resolved}': {why}"));
        return true;
    }
    job.say(&format!("   [Google Drive] 🔽 Downloading as '{resolved}'..."));
    match download(job, id.as_deref(), if id.is_none() { Some(url) } else { None }, Output::File(output), false, true) {
        Ok(p) => {
            let p = PathBuf::from(p);
            if !p.is_file() {
                job.say(&format!("   [Google Drive] ❌ Downloaded file not found at expected path: {}", p.to_string_lossy()));
                return false;
            }
            if quota_page(&p) {
                let _ = std::fs::remove_file(&p);
                job.say(QUOTA_MSG);
                return false;
            }
            job.say(&format!("   [Google Drive] ✅ Download complete to: {}", p.to_string_lossy()));
            true
        }
        Err(e) => {
            let s = e.text();
            if s.contains("Too many users have viewed or downloaded this file recently") || s.contains("Quota exceeded") {
                job.say(QUOTA_MSG);
            } else if s.contains("Cancelled by user") || s.contains("InterruptedError") {
                job.say("   [Google Drive] ℹ️ Download cancelled.");
            } else if s.contains("Cannot retrieve the public link") || s.contains("Access denied") || s.contains("Permission") {
                job.say("   [Google Drive] ⚠️ Access Denied: File may be private or requires Google account permissions.");
            } else {
                job.say(&format!("   [Google Drive] ❌ Download error: {s}"));
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::cloud::{Resp, Web};
    use super::super::filters::Options;
    use super::*;
    use base64::Engine;
    use serde_json::{json, Map, Value};
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    /// The made-up Drive: [url, status, headers, body (base64)]; redirects followed
    struct Site(Vec<Value>);

    impl Site {
        fn once(&self, url: &str, range: Option<&str>) -> Resp {
            for s in &self.0 {
                if s[0] == url {
                    let body = base64::engine::general_purpose::STANDARD.decode(s[3].as_str().unwrap()).unwrap();
                    let mut headers: HashMap<String, String> = s[2].as_object().unwrap().iter().map(|(k, v)| (k.to_lowercase(), v.as_str().unwrap().to_string())).collect();
                    if let (Some(true), Some(r)) = (s.get(4).and_then(Value::as_bool), range) {
                        let start: usize = r[6..r.len() - 1].parse().unwrap();
                        headers.insert("content-range".into(), format!("bytes {start}-{}/{}", body.len() - 1, body.len()));
                        headers.insert("content-length".into(), (body.len() - start).to_string());
                        return Resp { status: 206, headers, url: url.into(), body: Box::new(std::io::Cursor::new(body[start..].to_vec())) };
                    }
                    return Resp { status: s[1].as_u64().unwrap() as u16, headers, url: url.into(), body: Box::new(std::io::Cursor::new(body)) };
                }
            }
            Resp { status: 404, headers: HashMap::new(), url: url.into(), body: Box::new(std::io::empty()) }
        }
    }

    impl Web for Site {
        fn get(&self, url: &str, h: &[(&str, String)]) -> Result<Resp, String> {
            let range = h.iter().find(|(k, _)| *k == "Range").map(|(_, v)| v.as_str());
            let mut u = url.to_string();
            for _ in 0..10 {
                let r = self.once(&u, range);
                match r.headers.get("location").filter(|_| (300..400).contains(&r.status)) {
                    Some(l) => u = l.clone(),
                    None => return Ok(r),
                }
            }
            Err("redirects".into())
        }
        fn post(&self, _: &str, _: &Value) -> Result<Resp, String> {
            Err("no".into())
        }
        fn get_no_follow(&self, url: &str) -> Result<Resp, String> {
            Ok(self.once(url, None))
        }
    }

    fn tree(base: &Path) -> Map<String, Value> {
        let mut out = Map::new();
        fn walk(d: &Path, base: &Path, out: &mut Map<String, Value>) {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                    let rel = re!(r"[a-z0-9_]{8}\.part$").replace(&rel, "????????.part").to_string();
                    out.insert(rel, json!(base64::engine::general_purpose::STANDARD.encode(std::fs::read(&p).unwrap())));
                }
            }
        }
        walk(base, base, &mut out);
        out
    }

    #[test]
    fn matches_python_gdown() {
        let fx: Value = serde_json::from_str(include_str!("../../tests/fixtures/gdrive.json")).unwrap();
        if cfg!(windows) {
            for s in fx["sanitized"].as_array().unwrap() {
                assert_eq!(sanitize(s[0].as_str().unwrap()), s[1].as_str().unwrap(), "{s}");
            }
        }
        let opts = Options::from_dict(&fx["options"]);
        for (i, c) in fx["cases"].as_array().unwrap().iter().enumerate() {
            let site = Site(c["site"].as_array().unwrap().clone());
            let logs = Mutex::new(Vec::<String>::new());
            let log = |s: &str| logs.lock().unwrap().push(s.to_string());
            let cancel = AtomicBool::new(false);
            let job = Job { web: &site, log: &log, progress: &|_, _, _, _, _| {}, cancel: &cancel, skip_words: opts.str("skip_words"),
                            options: Some(opts.clone()), workers: 1 };
            let base = std::env::temp_dir().join(format!("pawchive_gdrive_{}_{i}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let target = base.join("dl");
            for (name, data) in c["setup"].as_object().unwrap() {
                std::fs::create_dir_all(&target).unwrap();
                std::fs::write(target.join(name), base64::engine::general_purpose::STANDARD.decode(data.as_str().unwrap()).unwrap()).unwrap();
            }
            let url = c["url"].as_str().unwrap();
            let ok = gdrive(&job, url, &target);
            let t = target.to_string_lossy().to_string();
            let got: Vec<String> = logs.lock().unwrap().iter().map(|l| re!(r"[a-z0-9_]{8}\.part").replace_all(&l.replace(&t, "<T>"), "????????.part").to_string()).collect();
            let want: Vec<String> = c["logs"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect();
            assert_eq!(got, want, "logs of {url}");
            assert_eq!(ok, c["ok"].as_bool().unwrap(), "result of {url}");
            assert_eq!(Value::Object(tree(&target)), c["files"], "files of {url}");
            let _ = std::fs::remove_dir_all(&base);
        }
    }

    #[test]
    fn wraps_like_python() {
        let t = "Cannot retrieve the public link of the file. You may need to change the permission to 'Anyone with the link', or have had many accesses. Check FAQ in https://github.com/wkentaro/gdown?tab=readme-ov-file#faq.";
        assert_eq!(wrap(t), vec![
            "Cannot retrieve the public link of the file. You may need to change",
            "the permission to 'Anyone with the link', or have had many accesses.",
            "Check FAQ in https://github.com/wkentaro/gdown?tab=readme-ov-file#faq.",
        ]);
        let t2 = "Too many users have viewed or downloaded this file recently. Please try accessing the file again later. If the file you are trying to access is particularly large or is shared with many people, it may take up to 24 hours to be able to view or download the file.";
        assert_eq!(wrap(t2), vec![
            "Too many users have viewed or downloaded this file recently. Please",
            "try accessing the file again later. If the file you are trying to",
            "access is particularly large or is shared with many people, it may",
            "take up to 24 hours to be able to view or download the file.",
        ]);
        let t3 = format!("aaaa-{}-cccccccc dd-ee x-y well-known co-op e-mail abc--def {}", "b".repeat(65), "z".repeat(80));
        assert_eq!(wrap(&t3), vec![
            "aaaa-".to_string(),
            format!("{}-", "b".repeat(65)),
            format!("cccccccc dd-ee x-y well-known co-op e-mail abc--def {}", "z".repeat(18)),
            "z".repeat(62),
        ]);
    }
}
