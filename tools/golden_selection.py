"""Writes the reference answers of the Python post picker (core/post_selection.py) for the Rust
port's test (src-tauri/src/downloader/selection.rs).

    python tools/golden_selection.py

Output: desktop/src-tauri/tests/fixtures/selection.json — preview cards of made-up posts on each
site, and the posts kept for picked posts and files.
"""

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core import post_selection  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "selection.json")

POSTS = [
    # Pawchive: main file and attachments, one listed twice, a cover, a video, a zip
    {"id": 101, "title": "Summer &amp; set", "published": "2026-05-01T10:00:00",
     "content": "<p>Hello<br/>world &amp; more</p>", "file": {"name": "cover.png", "path": "/aa/bb/aabb11.png"},
     "attachments": [{"name": "full.png", "path": "/data/aa/bb/aabb11.png"}, {"name": "clip.mp4", "path": "/cc/dd/ccdd.mp4"},
                     {"name": "pack.zip", "path": "/ee/ff/eeff.zip"}, {"name": "b.JPG?x", "path": "/11/22/b.jpg?token=1"}]},
    # No title: the first line of the text; no file; attachments without names
    {"id": "102", "title": "", "content": "First line here\nsecond", "published": None, "file": {},
     "attachments": [{"path": "/12/34/abc.webp"}, {"path": "/12/34/v.MOV"}, "junk", {"name": "x"}]},
    # cum.st: storage keys, mime types, temp links
    {"id": 103, "title": "Fans", "published": "2026-08-23", "captionHtml": "Hi <b>there</b>",
     "file": {"name": "img.jpeg", "storageKey": "ab12cd34ef56ab78cd90", "mimeType": "image/jpeg"},
     "attachments": [{"name": "movie", "storageKey": "ff00ff00ff00ff00ff00", "mimeType": "video/mp4"},
                     {"name": "tmp.mp4", "path": "/aa/aa/t.mp4", "temp_url": "https://x/t.mp4"},
                     {"name": "photo", "storageKey": "1234567890abcdef1234", "mimeType": "image/png"},
                     {"name": "dup.jpeg", "storageKey": "ab12cd34ef56ab78cd90"}]},
    # Absolute links and a preview listed twice
    {"id": 104, "title": "Links", "caption": "a &lt;b&gt;",
     "file": {"name": "", "path": "https://cdn.example.com/files/a.gif"},
     "attachments": [{"name": "preview.gif", "path": "https://cdn.example.com/files/a.gif"},
                     {"name": "vid.webm", "path": "https://cdn.example.com/files/v.webm"}]},
    {"id": 105, "title": "日本語のタイトル", "published": "2026-01-02", "attachments": None,
     "file": {"name": "ü.png", "path": "aa/bb/u.png"}},
    {"title": "No id", "file": {"path": "/a1/b2/c3.bmp"}},
    # cum.st as it is now: no file, attachments with only an original name and a storage key
    {"id": "2692395466", "published": 1787486459, "file": None, "attachments": [
        {"id": "66ad", "kind": "image", "mimeType": "image/jpeg", "storageKey": "e5ba00375f0362614105f0fa1033c4c4",
         "originalFilename": "3152x4203_70f0.jpg"},
        {"id": "cdcc", "kind": "video", "mimeType": "video/mp4", "storageKey": "a52985f3b400654b7cbeea87bb413ea8",
         "originalFilename": "clip.mp4"}]},
]

DOMAINS = ["pawchive.pw", "cum.st", "", "kemono.su"]

PICKS = [
    ({"ids": ["101", "103"], "files": {}}),
    ({"ids": ["101"], "files": {"101": ["/aa/bb/aabb11.png", "pack.zip"]}}),
    ({"ids": ["101", "102"], "files": {"102": ["\\12\\34\\abc.webp"], "101": "not a list"}}),
    ({"ids": ["103"], "files": {"103": ["ab12cd34ef56ab78cd90", "/data/aa/aa/t.mp4"]}}),
    ({"ids": ["104", "105", "999"], "files": {"104": ["V.WEBM"], "105": []}}),
    ({"ids": [], "files": {}}),
    ({"ids": ["2692395466"], "files": {"2692395466": ["a52985f3b400654b7cbeea87bb413ea8"]}}),
]


def main():
    cards = [[d, post_selection.cards(POSTS, d)] for d in DOMAINS]
    picks = [[p["ids"], p["files"], post_selection.picked(POSTS, p["ids"], p["files"])] for p in PICKS]
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump({"posts": POSTS, "cards": cards, "picks": picks}, fh, ensure_ascii=False, indent=1)
    print(f"{len(cards)} card sets, {len(picks)} picks -> {FIXTURE}")


if __name__ == "__main__":
    main()
