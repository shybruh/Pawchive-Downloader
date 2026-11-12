"""Writes the reference answers of the embedded-video downloader (services/ytdlp_manager.py and
the yt-dlp parts of core/downloader.py) for the Rust port's test (src-tauri/src/downloader/ytdlp.rs).

    python tools/golden_ytdlp.py

Cases: yt-dlp's command line, its progress lines, its error text, the browser sign-in errors, and
the messages for videos that can't be downloaded.
Output: desktop/src-tauri/tests/fixtures/ytdlp.json
"""

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from services import ytdlp_manager as ym  # noqa: E402
from core import downloader as dl  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "ytdlp.json")

COMMANDS = [
    ["C:\\deps\\yt-dlp.exe", "https://youtu.be/abc", "C:\\out\\Post", "video.mp4", 0, "", ""],
    ["C:\\deps\\yt-dlp.exe", "https://youtu.be/abc", "C:\\out\\Post", "", 0, "", ""],
    ["C:\\deps\\yt-dlp.exe", "https://youtu.be/abc", "C:\\out\\Post", None, 0, "", ""],
    ["C:\\deps\\yt-dlp.exe", "https://player.VIMEO.com/video/1", "C:\\out", "clip.name.webm", 10485760, "https://www.patreon.com/", "firefox"],
    ["C:\\deps\\yt-dlp.exe", "https://youtu.be/x", "C:\\out", "noext", 1, "https://www.patreon.com/", "chrome"],
    ["C:\\deps\\yt-dlp.exe", "https://youtu.be/x", "C:\\out", ".hidden", 0, "", ""],
]

PROGRESS = [
    "PROGRESS:1024/2048:512.0:3", "PROGRESS:NA/NA:NA:NA", "PROGRESS:1500.5/NA:2048.7:61", "PROGRESS:10/20:1048577:3600",
    "PROGRESS:10/20:1048576:60", "PROGRESS:10/20:1024:0", "PROGRESS:10/20:1025:121", "PROGRESS:10:5:5",
    "PROGRESS:10/20:5", "PROGRESS:a/b:c:d", "  PROGRESS:5/10:1:1  \n", "[download] 10%", "", "PROGRESS:",
    "PROGRESS:5/10/15:1:1:extra", "PROGRESS:5/:1:1", "PROGRESS:/10:1:1", "PROGRESS:1e3/2e3:1e6:1e2",
    "PROGRESS:-5/10:-1:-5", "PROGRESS:5/10:inf:1", "PROGRESS:5/10:nan:1",
]

ERRORS = [
    "", "ERROR: [youtube] abc: Video unavailable", "WARNING: x\nERROR: first\nERROR: second  ",
    "plain failure", "x" * 200, "ERROR:" + "y" * 300, "ERROR:", "yt-dlp exited with code 1",
]

COOKIES = [
    "", "ERROR: Could not copy Chrome cookie database. See ...", "failed to decrypt with DPAPI",
    "Unsupported browser specified for cookies: 'x'", "ERROR: Sign in to confirm you're not a bot", "KEYRING error",
    "cookies database is locked",
]

MESSAGES = [
    ["", ""], ["Unsupported URL: https://example.com", ""], ["There's no video in this post", ""],
    ["HTTP Error 410: Gone", "https://www.redgifs.com/watch/x"], ["410", "https://redgifs.com/x"],
    ["Gone", "https://v3.REDGIFS.com/x"], ["whatever", "https://gfycat.com/abc"], ["gfycat.com is down", ""],
    ["Private video", "https://youtu.be/x"], ["HTTP Error 404: Not Found", "https://youtu.be/x"],
    ["No media found", "https://x"], ["ERROR redgifs 404", "https://redgifs.com/x"],
]


def main():
    out = {
        "commands": [[c, ym.build_command(*c)] for c in COMMANDS],
        "progress": [[p, list(ym.parse_progress_line(p)) if ym.parse_progress_line(p) else None] for p in PROGRESS],
        "errors": [[e, ym.short_error(e)] for e in ERRORS],
        "cookies": [[c, ym.is_cookie_error(c)] for c in COOKIES],
        "messages": [[m, u, dl.is_not_a_video_error(m), dl.friendly_embed_error(m, u)] for m, u in MESSAGES],
        "browsers": list(ym.COOKIE_BROWSERS),
        "referers": dl.EMBED_REFERERS,
    }
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"{len(COMMANDS)} commands, {len(PROGRESS)} progress lines, {len(ERRORS)} errors -> {FIXTURE}")
    for p, r in out["progress"]:
        print(repr(p), r)


if __name__ == "__main__":
    main()
