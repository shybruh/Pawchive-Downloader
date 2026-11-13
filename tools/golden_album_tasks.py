"""Writes the reference answers of the album sites' queue files (bridge/app_bridge.py album_tasks)
for the Rust port's test (src-tauri/src/downloader/scan.rs album_tasks).

    python tools/golden_album_tasks.py

Cases: Bunkr, Erome and nHentai albums with made-up titles (HTML entities, characters not allowed
in folder names, empty, very long) and files (the same name twice, names differing only in case),
into a few download folders.
Output: desktop/src-tauri/tests/fixtures/album_tasks.json
"""

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from bridge.app_bridge import album_tasks  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "album_tasks.json")

FILES = [
    {"url": "https://cdn.example/a1.jpg", "filename": "a.jpg", "size": 1200},
    {"url": "https://cdn.example/a2.jpg", "filename": "a.jpg", "size": 1300},
    {"url": "https://cdn.example/a3.jpg", "filename": "A.JPG"},
    {"url": "https://cdn.example/b.mp4", "filename": "clip.final.mp4", "size": 0},
    {"url": "https://cdn.example/c", "filename": "noext"},
    {"url": "https://cdn.example/c2", "filename": "noext"},
    {"url": "https://cdn.example/d.png", "filename": "a (2).jpg", "size": 5},
    {"url": "https://cdn.example/e.gif", "filename": "日本語 ファイル.gif", "size": 77},
]
TITLES = ["My Album", "Tom &amp; Jerry&#39;s &quot;best&quot;", "bad:name/with\\chars*?", "", None, "  spaced  out  ",
          "x" * 300, "CON", "trailing dots...", "Ã©mojis ✨ and Ã¼mlauts"]
DIRS = [r"C:\Users\me\Downloads\Kemono", "C:\\dl\\", "D:/mixed/slashes", ""]


def main():
    cases = []
    for provider in ("bunkr", "erome", "nhentai"):
        for i, title in enumerate(TITLES):
            base = DIRS[i % len(DIRS)]
            post_id = ["", "abc123", "556677"][i % 3]
            name, tasks = album_tasks(provider, title, FILES, base, post_id)
            cases.append({"provider": provider, "title": title, "base_dir": base, "post_id": post_id,
                          "creator_name": name, "tasks": tasks})
    with open(FIXTURE, "w", encoding="utf-8", newline="\n") as f:
        json.dump({"files": FILES, "cases": cases}, f, ensure_ascii=False, indent=1)
    print(f"wrote {FIXTURE}: {len(cases)} albums")


if __name__ == "__main__":
    main()
