//! Paths saved by the app on the other operating system (dual boot with a shared drive, or the
//! same data used from Windows and WSL): "D:\some\folder" on Windows, "/mnt/d/some/folder" (WSL) or
//! wherever the drive is mounted on Linux ("/media/me/DATA/…", "/run/media/me/DATA/…"). Saved paths
//! are translated to this system's form when they're read; other paths are left as they are.
//! A port of core/path_translation.py, checked against it by tools/golden_path_translation.py.
//!
//! - On Linux, a Windows drive is looked for at WSL's "/mnt/<drive>" first, then among the mounted
//!   Windows drives (NTFS / exFAT / FAT): the one where the path's folders exist. settings.json can
//!   set a fixed pattern ("windows_drive_mount", e.g. "/media/me/{drive}").
//! - On Windows, "/mnt/<letter>/…" is that drive; "/media/<user>/<name>/…",
//!   "/run/media/<user>/<name>/…" and "/mnt/<name>/…" are the drive whose volume label is <name>
//!   (or where the path's folders exist).

use regex::Regex;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the translation looks at on this computer
pub trait System: Send {
    /// The program runs on Windows
    fn is_windows(&self) -> bool;
    fn exists(&self, path: &str) -> bool;
    fn is_dir(&self, path: &str) -> bool;
    /// Mount points of Windows-format drives (Linux)
    fn linux_mounts(&self) -> Vec<String>;
    /// {volume label (lower case): "D:"} (Windows)
    fn windows_labels(&self) -> HashMap<String, String>;
}

const MISS_TTL: Duration = Duration::from_secs(60);
const WINDOWS_FS: &[&str] = &["ntfs", "ntfs3", "fuseblk", "exfat", "vfat", "msdos", "9p", "drvfs"];

#[derive(Clone)]
enum Hit {
    Found(String),
    Missed(Instant),
}

pub struct Translator {
    pub mount_pattern: String,
    sys: Box<dyn System>,
    // Drives found: a lookup costs file-system calls, and a big queue saved on the other system has
    // hundreds of thousands of paths. Not found: looked for again after a minute (drive plugged in).
    found: HashMap<Vec<String>, Hit>,
}

fn key(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn first_part(rest: &str) -> &str {
    rest.split('/').next().unwrap_or("")
}

fn re_linux_mount() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(?:/run/media/[^/]+|/media/[^/]+|/media|/mnt)/([^/]+)(?:/(.*))?$").unwrap())
}

fn re_win_drive() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^([A-Za-z]):(?:[\\/](.*))?$").unwrap())
}

impl Translator {
    pub fn new(sys: Box<dyn System>) -> Self {
        Self { mount_pattern: "/mnt/{drive}".into(), sys, found: HashMap::new() }
    }

    pub fn clear_cache(&mut self) {
        self.found.clear();
    }

    /// _cached: a remembered answer (a miss for a minute), else `find`'s
    fn cached(&mut self, k: Vec<String>, find: &mut dyn FnMut(&dyn System) -> Option<String>) -> Option<String> {
        match self.found.get(&k) {
            Some(Hit::Found(f)) => return Some(f.clone()),
            Some(Hit::Missed(at)) if at.elapsed() <= MISS_TTL => return None,
            _ => {}
        }
        let got = find(self.sys.as_ref());
        if self.found.len() > 4096 {
            self.found.clear();
        }
        self.found.insert(k, match &got {
            Some(f) if !f.is_empty() => Hit::Found(f.clone()),
            _ => Hit::Missed(Instant::now()),
        });
        got.filter(|f| !f.is_empty())
    }

    /// _found_on: a drive once found is kept for every path on it; a miss is remembered per first folder
    fn found_on(&mut self, drive_key: Vec<String>, first: &str, find: &mut dyn FnMut(&dyn System) -> Option<String>) -> Option<String> {
        if let Some(Hit::Found(f)) = self.found.get(&drive_key) {
            return Some(f.clone());
        }
        let mut k = drive_key.clone();
        k.push(first.to_lowercase());
        let got = self.cached(k, find);
        if let Some(f) = &got {
            self.found.insert(drive_key, Hit::Found(f.clone()));
        }
        got
    }

    fn linux_drive_regex(&self) -> Regex {
        let (head, tail) = match self.mount_pattern.split_once("{drive}") {
            Some((h, t)) => (h.to_string(), t.to_string()),
            None => (self.mount_pattern.clone(), String::new()),
        };
        Regex::new(&format!("^{}([A-Za-z]){}(?:/(.*))?$", regex::escape(&head), regex::escape(&tail))).unwrap()
    }

    /// Where Windows drive <letter> is on this Linux system
    fn linux_root(&mut self, letter: &str, rest: &str) -> String {
        let letter = letter.to_lowercase();
        let pattern_root = self.mount_pattern.replace("{drive}", &letter);
        if self.mount_pattern != "/mnt/{drive}" {
            return pattern_root;
        }
        let root = pattern_root.clone();
        if self.cached(key(&["wsl", &letter]), &mut |s| s.is_dir(&root).then(|| root.clone())).is_some() {
            return pattern_root;
        }
        let first = first_part(rest).to_string();
        let mut find = |s: &dyn System| {
            if first.is_empty() {
                return None;
            }
            s.linux_mounts().into_iter().find(|m| s.exists(&format!("{}/{first}", m.trim_end_matches('/'))))
        };
        self.found_on(key(&["linux", &letter]), &first, &mut find).unwrap_or(pattern_root)
    }

    /// The drive a Linux mount named <name> stands for: by volume label, else where the path's
    /// folders exist
    fn windows_drive_for(&mut self, name: &str, rest: &str) -> Option<String> {
        let first = first_part(rest).to_string();
        let lname = name.to_lowercase();
        let mut find = |s: &dyn System| {
            if let Some(d) = s.windows_labels().get(&lname) {
                return Some(d.clone());
            }
            if !first.is_empty() {
                for letter in b'C'..=b'Z' {
                    if s.exists(&format!("{}:\\{first}", letter as char)) {
                        return Some(format!("{}:", letter as char));
                    }
                }
            }
            None
        };
        self.found_on(key(&["windows", &lname]), &first, &mut find)
    }

    /// The path as the system named for (`windows`; None: this one) names it; anything else unchanged
    pub fn localize(&mut self, path: &str, windows: Option<bool>) -> String {
        if path.is_empty() {
            return String::new();
        }
        if windows.unwrap_or_else(|| self.sys.is_windows()) {
            if let Some(m) = self.linux_drive_regex().captures(path) {
                let rest = m.get(2).map_or("", |g| g.as_str()).trim_matches('/');
                return format!("{}:\\{}", m[1].to_uppercase(), rest.replace('/', "\\"));
            }
            if self.sys.is_windows() {
                if let Some(m) = re_linux_mount().captures(path) {
                    let rest = m.get(2).map_or("", |g| g.as_str()).trim_matches('/').to_string();
                    let name = m[1].to_string();
                    if let Some(drive) = self.windows_drive_for(&name, &rest) {
                        return format!("{drive}\\{}", rest.replace('/', "\\"));
                    }
                }
            }
            return path.to_string();
        }
        if let Some(m) = re_win_drive().captures(path) {
            let rest = m.get(2).map_or("", |g| g.as_str()).replace('\\', "/").trim_matches('/').to_string();
            let root = self.linux_root(&m[1], &rest);
            return if rest.is_empty() { root } else { format!("{root}/{rest}") };
        }
        path.to_string()
    }
}

/// This computer
struct Here;

impl System for Here {
    fn is_windows(&self) -> bool {
        cfg!(windows)
    }
    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }
    fn is_dir(&self, path: &str) -> bool {
        std::path::Path::new(path).is_dir()
    }
    fn linux_mounts(&self) -> Vec<String> {
        let Ok(t) = std::fs::read_to_string("/proc/mounts") else { return Vec::new() };
        t.lines()
            .filter_map(|l| {
                let p: Vec<&str> = l.split_whitespace().collect();
                (p.len() >= 3 && WINDOWS_FS.contains(&p[2].to_lowercase().as_str())).then(|| p[1].replace("\\040", " "))
            })
            .collect()
    }
    #[cfg(windows)]
    fn windows_labels(&self) -> HashMap<String, String> {
        use windows_sys::Win32::Storage::FileSystem::{GetLogicalDrives, GetVolumeInformationW};
        let mut out = HashMap::new();
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain([0]).collect();
            let mut buf = [0u16; 261];
            let ok = unsafe {
                GetVolumeInformationW(root.as_ptr(), buf.as_mut_ptr(), 261, std::ptr::null_mut(), std::ptr::null_mut(),
                                      std::ptr::null_mut(), std::ptr::null_mut(), 0)
            };
            if ok != 0 {
                let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
                let label = String::from_utf16_lossy(&buf[..n]);
                if !label.is_empty() {
                    out.insert(label.to_lowercase(), format!("{letter}:"));
                }
            }
        }
        out
    }
    #[cfg(not(windows))]
    fn windows_labels(&self) -> HashMap<String, String> {
        HashMap::new()
    }
}

fn global() -> &'static Mutex<Translator> {
    static T: OnceLock<Mutex<Translator>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Translator::new(Box::new(Here))))
}

/// A saved path in this system's form
pub fn localize_path(path: &str) -> String {
    global().lock().unwrap().localize(path, None)
}

/// settings.json's "windows_drive_mount" (a pattern with "{drive}")
pub fn set_mount_pattern(pattern: &str) {
    if pattern.contains("{drive}") {
        let mut t = global().lock().unwrap();
        t.mount_pattern = pattern.to_string();
        t.clear_cache();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// The golden script's made-up computer
    struct World {
        dirs: Vec<String>,
        mounts: Vec<String>,
        labels: HashMap<String, String>,
        windows: bool,
    }

    fn norm(p: &str) -> String {
        if p.as_bytes().get(1) == Some(&b':') {
            p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
        } else {
            p.trim_end_matches('/').to_string()
        }
    }

    impl System for World {
        fn is_windows(&self) -> bool {
            self.windows
        }
        fn exists(&self, path: &str) -> bool {
            let n = norm(path);
            let sep = if n.as_bytes().get(1) == Some(&b':') { "\\" } else { "/" };
            self.dirs.iter().any(|d| {
                let d = norm(d);
                d == n || d.starts_with(&format!("{n}{sep}"))
            })
        }
        fn is_dir(&self, path: &str) -> bool {
            self.exists(path)
        }
        fn linux_mounts(&self) -> Vec<String> {
            self.mounts.clone()
        }
        fn windows_labels(&self) -> HashMap<String, String> {
            self.labels.clone()
        }
    }

    #[test]
    fn matches_python() {
        let fx: Value = serde_json::from_str(include_str!("../tests/fixtures/path_translation.json")).unwrap();
        let w = &fx["world"];
        let list = |v: &Value| v.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>();
        let mut n = 0;
        for c in fx["cases"].as_array().unwrap() {
            let world = World {
                dirs: list(&w["dirs"]),
                mounts: list(&w["linux_mounts"]),
                labels: w["labels"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect(),
                windows: c["host_windows"].as_bool().unwrap(),
            };
            let mut t = Translator::new(Box::new(world));
            t.mount_pattern = c["pattern"].as_str().unwrap().to_string();
            let windows = c["windows"].as_bool().unwrap();
            for a in c["answers"].as_array().unwrap() {
                let (p, want) = (a[0].as_str().unwrap(), a[1].as_str().unwrap());
                assert_eq!(t.localize(p, Some(windows)), want, "{p:?} for windows={windows}, pattern {}", t.mount_pattern);
                n += 1;
            }
        }
        assert_eq!(n, 102);
    }
}
