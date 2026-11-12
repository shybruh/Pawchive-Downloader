"""Writes the reference answers of core/path_translation.py for the Rust port's test
(src-tauri/src/path_translation.rs).

    python tools/golden_path_translation.py

The system is made up (which folders exist, the mounted Windows drives on Linux, the volume labels
on Windows), so the answers don't depend on this computer. Each case: the system named for (Windows
or Linux), the mount pattern, the path, the answer.
Output: desktop/src-tauri/tests/fixtures/path_translation.json
"""

import json
import ntpath
import os
import posixpath
import sys
from types import SimpleNamespace

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core import path_translation as pt  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "path_translation.json")

# The made-up system
WORLD = {
    # folders that exist (both forms; Windows ones compared without case)
    "dirs": ["/mnt/c", "/mnt/e", "/media/me/DATA/Downloads", "/run/media/me/Games/steam", "/mnt/Stuff/Kemono",
             "D:\\Kemono", "F:\\Downloads", "G:\\Music"],
    "linux_mounts": ["/media/me/DATA", "/run/media/me/Games", "/mnt/Stuff", "/mnt/c", "/mnt/e"],
    "labels": {"data": "D:", "games": "G:", "stuff": "F:"},
}


def fake_os(nt: bool):
    dirs = WORLD["dirs"]

    def norm(p):
        return p.replace("/", "\\").rstrip("\\").lower() if (len(p) > 1 and p[1] == ":") else p.rstrip("/")

    def exists(p):
        n = norm(p)
        return any(n == norm(d) or norm(d).startswith(n + ("\\" if n[1:2] == ":" else "/")) for d in dirs)

    return SimpleNamespace(name="nt" if nt else "posix",
                           path=SimpleNamespace(isdir=exists, exists=exists, join=posixpath.join))


LINUX_PATHS = ["/mnt/d/Kemono/a b", "/mnt/D/x", "/mnt/c", "/mnt/c/", "/mnt/e//deep//x/", "/mnt/stuff/Kemono/file.jpg",
               "/media/me/DATA/Downloads/Alice", "/media/me/data/x", "/run/media/me/Games/steam/y", "/media/DATA/Downloads",
               "/media/me/Unknown/Kemono", "/media/me/Unknown/Music/z", "/mnt/Stuff", "/mnt/Nope/Kemono/z",
               "/home/me/x", "relative/path", "", "C:\\already\\windows", "/mnt/cc/x", "/mnt/1/x"]
WINDOWS_PATHS = ["D:\\Kemono\\a b", "d:/Kemono/x", "C:", "C:\\", "E:\\deep\\\\x\\", "Z:\\Downloads\\Alice",
                 "D:\\", "C:/Users/me", "/mnt/c/x", "\\\\server\\share\\x", "relative\\x", "", "DATA:\\x", "x:\\Downloads"]


def run(windows: bool, host_nt: bool, pattern: str, paths):
    pt.mount_pattern = pattern
    pt._found_mounts.clear()
    pt.os = fake_os(host_nt)
    pt._linux_mounts = lambda: list(WORLD["linux_mounts"])
    pt._windows_labels = lambda: dict(WORLD["labels"])
    return [[p, pt.localize_path(p, windows=windows)] for p in paths]


def main():
    real_os, real_mounts, real_labels = pt.os, pt._linux_mounts, pt._windows_labels
    try:
        cases = []
        for windows, host_nt, pattern, paths in [
            (True, True, "/mnt/{drive}", LINUX_PATHS),
            (True, True, "/media/me/{drive}", LINUX_PATHS),
            (True, False, "/mnt/{drive}", LINUX_PATHS),
            (False, False, "/mnt/{drive}", WINDOWS_PATHS),
            (False, False, "/media/me/{drive}", WINDOWS_PATHS),
            (False, False, "/x/{drive}/y", WINDOWS_PATHS),
        ]:
            cases.append({"windows": windows, "host_windows": host_nt, "pattern": pattern,
                          "answers": run(windows, host_nt, pattern, paths)})
    finally:
        pt.os, pt._linux_mounts, pt._windows_labels = real_os, real_mounts, real_labels
        pt.mount_pattern = "/mnt/{drive}"
    with open(FIXTURE, "w", encoding="utf-8", newline="\n") as f:
        json.dump({"world": WORLD, "cases": cases}, f, ensure_ascii=False, indent=1)
    print(f"wrote {FIXTURE}: {sum(len(c['answers']) for c in cases)} answers")


if __name__ == "__main__":
    main()
