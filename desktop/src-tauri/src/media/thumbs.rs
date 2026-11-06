//! Thumbnails (cached on disk as small JPEGs) and full-size decoding for formats browsers can't show.
//!
//! The cache folder and file names match what the Python engine wrote, so thumbnails made by older
//! versions are reused.

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageReader, Rgb, RgbImage, RgbaImage};
use sha1::{Digest, Sha1};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Thumbnails fit inside this box (the UI crops them to its cards).
const THUMB_BOX: u32 = 480;
const CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const CACHE_PRUNE_TARGET: u64 = 400 * 1024 * 1024;
/// Transparent pictures are flattened onto the card colour (the cache is JPEG)
const BACKGROUND: [u8; 3] = [0x13, 0x18, 0x24];

const VIDEO: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "flv", "f4v", "wmv", "asf", "mpg", "mpeg", "m2v", "ts", "mts", "m2ts",
    "3gp", "3g2", "ogv", "vob", "divx",
];

pub struct ThumbCache {
    dir: PathBuf,
}

/// Where Qt kept its cache for this app (organisation "PawchiveProject", app "PawchiveDownloader").
fn cache_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Caches"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
    let base = base.unwrap_or_else(std::env::temp_dir);
    #[cfg(windows)]
    let dir = base.join("PawchiveProject").join("PawchiveDownloader").join("cache").join("thumbnails");
    #[cfg(not(windows))]
    let dir = base.join("PawchiveProject").join("PawchiveDownloader").join("thumbnails");
    dir
}

/// Python's os.path.normcase(os.path.normpath(p)), which named the cached files.
fn normalized(path: &Path) -> String {
    let s = path.to_string_lossy();
    #[cfg(windows)]
    {
        let s = s.replace('/', "\\");
        let (prefix, rest) = match s.find(":\\") {
            Some(i) if i == 1 => (s[..3].to_string(), s[3..].to_string()),
            _ => (String::new(), s.clone()),
        };
        let mut parts: Vec<&str> = Vec::new();
        for part in rest.split('\\') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        format!("{prefix}{}", parts.join("\\")).to_lowercase()
    }
    #[cfg(not(windows))]
    {
        s.into_owned()
    }
}

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

impl ThumbCache {
    pub fn open() -> Self {
        let dir = cache_dir();
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn cached_path(&self, path: &Path) -> Option<PathBuf> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime_ns = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
        let key = format!("{}|{}|{}|{}", normalized(path), mtime_ns, meta.len(), THUMB_BOX);
        let hash = hex(&Sha1::digest(key.as_bytes()));
        Some(self.dir.join(&hash[..2]).join(format!("{hash}.jpg")))
    }

    /// A JPEG thumbnail, from the cache or made now. None when there's no way to picture the file.
    pub fn thumbnail(&self, path: &Path) -> Option<Vec<u8>> {
        let cached = self.cached_path(path);
        if let Some(c) = &cached {
            if let Ok(bytes) = std::fs::read(c) {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
        }
        let img = if VIDEO.contains(&ext(path).as_str()) {
            shell(path, THUMB_BOX)
        } else {
            decode(path).or_else(|| shell(path, THUMB_BOX))
        }?;
        let img = fit(img, THUMB_BOX);
        let jpeg = to_jpeg(&flatten(img.to_rgba8()))?;
        if let Some(c) = cached {
            if let Some(parent) = c.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(c, &jpeg);
        }
        Some(jpeg)
    }

    /// Once per run: drop the oldest thumbnails when the cache has grown past its limit.
    pub fn prune_in_background(&self) {
        let dir = self.dir.clone();
        std::thread::spawn(move || {
            let mut entries = Vec::new();
            let mut total = 0u64;
            let mut stack = vec![dir];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let Ok(m) = e.metadata() else { continue };
                    if m.is_dir() {
                        stack.push(e.path());
                    } else {
                        total += m.len();
                        entries.push((m.modified().unwrap_or(UNIX_EPOCH), m.len(), e.path()));
                    }
                }
            }
            if total <= CACHE_MAX_BYTES {
                return;
            }
            entries.sort();
            for (_, size, p) in entries {
                if total <= CACHE_PRUNE_TARGET {
                    break;
                }
                if std::fs::remove_file(&p).is_ok() {
                    total -= size;
                }
            }
        });
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decodes a picture (whatever its extension says), turned the right way up.
fn decode(path: &Path) -> Option<DynamicImage> {
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Some(img)
}

fn fit(img: DynamicImage, max: u32) -> DynamicImage {
    if img.width() > max || img.height() > max {
        img.thumbnail(max, max)
    } else {
        img
    }
}

fn flatten(img: RgbaImage) -> RgbImage {
    RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let p = img.get_pixel(x, y).0;
        let a = p[3] as u32;
        let mix = |c: u8, bg: u8| ((c as u32 * a + bg as u32 * (255 - a)) / 255) as u8;
        Rgb([mix(p[0], BACKGROUND[0]), mix(p[1], BACKGROUND[1]), mix(p[2], BACKGROUND[2])])
    })
}

fn to_jpeg(img: &RgbImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85).encode_image(img).ok()?;
    Some(out)
}

/// The picture at full size as PNG, for formats a browser can't show (TIFF, HEIC with a codec…).
pub fn full_image_png(path: &Path) -> Option<Vec<u8>> {
    let img = decode(path).or_else(|| shell(path, 1600))?;
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// The thumbnail File Explorer shows (videos, and pictures only Windows can decode).
fn shell(path: &Path, size: u32) -> Option<DynamicImage> {
    #[cfg(windows)]
    {
        super::shell_thumb::thumbnail(path, size).map(DynamicImage::ImageRgba8)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, size);
        None
    }
}
