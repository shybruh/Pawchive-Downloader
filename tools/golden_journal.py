"""Writes the reference answers of the Python crash journal (core/queue_journal.py, with
core/recovery_manager.py's names) for the Rust port's test (src-tauri/src/downloader/journal.rs).

    python tools/golden_journal.py

Output: desktop/src-tauri/tests/fixtures/journal.json — the files (as the app sends them to the
engine), the rows the Python journal writes for them, platform / artist-name cases, and what reading
the journal back gives (the recovery prompt's check and summary, the saved queue, sizes as text).
"""

import json
import os
import sqlite3
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core.downloader import DownloadTask  # noqa: E402
from core.filter_engine import FilterOptions  # noqa: E402
from core.queue_journal import QueueJournal  # noqa: E402
from core.recovery_manager import RecoveryManager  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "journal.json")
B = "C:\\Downloads\\Pawchive"


def task(url, path, **kw):
    t = DownloadTask(url=url, target_path=os.path.join(B, path), post_title=kw.pop("post_title", "A post"),
                     creator_name=kw.pop("creator_name", "Artist"), service=kw.pop("service", "patreon"),
                     post_id=kw.pop("post_id", "1"), file_id=kw.pop("file_id", ""))
    for k, v in kw.items():
        setattr(t, k, v)
    return t


OPTS = FilterOptions()
OPTS2 = FilterOptions.from_dict({"file_type": "images", "skip_words": "wip", "subfolder_per_post": False})

TASKS = [
    task("https://pawchive.pw/data/aa/bb/x.png", "Artist [patreon]\\P\\x.png", file_id="f1", file_size=1000, options=OPTS),
    task("https://pawchive.pw/data/cc/dd/y.mp4", "Artist [patreon]\\P\\y.mp4", file_id="f2", status="completed",
         file_size=5000, downloaded_bytes=5000, expected_sha256="ab" * 32, options=OPTS2),
    task("https://cum.st/f/z.jpg", "Model [onlyfans]\\z.jpg", creator_name="Model", service="onlyfans", post_id="99",
         file_id="f3", status="failed", error_msg="HTTP 404", retry_count=2, http_status=404, used_preview=True,
         fallback_urls=["https://n2.cum.st/f/z.jpg"], batch_id="post_onlyfans_1_99", post_url="https://cum.st/x",
         post_date="2026-01-02", user_id="1"),
    task("https://bunkr.cr/v/abc", "Bunkr - Summer set\\a.mp4", creator_name="Bunkr", post_title="Untitled",
         service="bunkr", file_size=0, downloaded_bytes=1200),
    task("https://www.erome.com/a/1", "Erome - \\b.jpg", creator_name="Erome album", post_title="Holiday",
         service="erome"),
    task("tg://c/1/2", "Telegram\\Chan\\c.zip", creator_name="Chan", service="telegram", is_telegram=True,
         telegram_channel_id="-100123", telegram_message_id=42, status="paused"),
    task("https://youtube.com/watch?v=1", "Artist [patreon]\\P\\v.mp4", is_ytdlp=True, status="cancelled",
         error_msg="Download cancelled by user"),
    task("https://pawchive.pw/data/ee/ff/ü.png", "Ärtist [fanbox]\\ü.png", creator_name="Ärtist", service="fanbox",
         post_title="日本語", file_id="f8"),
    task("https://x/none", "", creator_name="", service="", post_title=""),
]


def payload(t, table, keys):
    o = getattr(t, "options", None)
    opt = ""
    if o is not None:
        opt = keys.get(id(o))
        if opt is None:
            opt = keys[id(o)] = f"o{len(keys) + 1}"
            table[opt] = o.to_dict()
    return {
        "key": "", "url": t.url, "target_path": t.target_path, "post_title": t.post_title, "creator_name": t.creator_name,
        "service": t.service, "post_id": str(t.post_id), "file_id": str(t.file_id), "file_size": int(t.file_size or 0),
        "expected_sha256": t.expected_sha256 or "", "is_ytdlp": bool(t.is_ytdlp), "is_telegram": bool(getattr(t, "is_telegram", False)),
        "telegram_channel_id": getattr(t, "telegram_channel_id", ""), "telegram_message_id": getattr(t, "telegram_message_id", 0),
        "batch_id": t.batch_id or "", "post_url": t.post_url or "", "post_date": t.post_date or "", "user_id": str(t.user_id or ""),
        "fallback_urls": list(t.fallback_urls or []), "post_info_path": "", "status": t.status, "error_msg": t.error_msg or "",
        "retry_count": int(t.retry_count or 0), "downloaded_bytes": int(t.downloaded_bytes or 0),
        "used_preview": bool(getattr(t, "used_preview", False)), "opt": opt,
    }


def main():
    rm = RecoveryManager(tempfile.mkdtemp())
    path = os.path.join(tempfile.mkdtemp(), "recovery_journal.db")
    j = QueueJournal(path)
    j.sync(TASKS, None, {"a": 1}, "in_progress", artist_of=rm.artist_name, platform_of=rm._platform_of)
    conn = sqlite3.connect(path)
    rows = [[k, pos, st, a, s, p, tb, d, json.loads(data), g] for (k, pos, st, a, s, p, tb, d, data, g) in conn.execute(
        "SELECT key, pos, status, artist, service, platform, total_bytes, downloaded, data, group_key FROM tasks ORDER BY pos;")]
    conn.close()
    table, keys = {}, {}
    platforms = [[s, u, RecoveryManager._detect_platform(s, url=u)] for s, u in [
        ("patreon", "https://pawchive.pw/x"), ("onlyfans", "https://cum.st/x"), ("fansly", ""), ("", ""), ("bunkr", ""),
        ("", "https://bunkr.cr/a"), ("", "https://balbums.st/a"), ("erome", ""), ("nhentai", "https://nhentai.net/g/1"),
        ("fanbox", "https://pawchive.pw/fanbox"), ("gumroad", "https://cum.st/g"), ("weird", "https://x"), ("candfans", "")]]
    artists = [[c, p, t, RecoveryManager.artist_name({"creator_name": c, "post_title": p, "target_path": t})] for c, p, t in [
        ("Artist", "", ""), ("", "", ""), ("Bunkr", "Summer", ""), ("Bunkr", "untitled", "C:\\d\\Bunkr - Set A\\x.mp4"),
        ("erome album", "", "C:\\d\\Erome - \\x.jpg"), ("nhentai", "", "C:\\d\\nHentai - 123\\x.jpg"),
        ("Unknown Artist", "Post", "C:\\d\\x\\y.jpg"), ("bunkr", "", "C:\\d\\Other\\y.jpg")]]
    # Reading it back: the recovery prompt's check and summary, and the saved queue
    summary = j.summary(rm._format_size)
    summary.pop("saved_at", None)
    loaded = j.load()
    loaded.pop("saved_at", None)
    sizes = [[n, RecoveryManager._format_size(n)] for n in (0, -5, 1, 512, 1023, 1024, 1536, 1100, 10 ** 6, 1288490188,
                                                              5 * 1024 ** 4, 3 * 1024 ** 5, 1024 ** 6, 1152, 1126)]
    out = {"tasks": [payload(t, table, keys) for t in TASKS], "options": table, "rows": rows,
           "platforms": platforms, "artists": artists, "summary": summary, "has_unfinished": j.has_unfinished(),
           "loaded": loaded, "sizes": sizes}
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"{len(rows)} rows, {len(platforms)} platforms, {len(artists)} artist names -> {FIXTURE}")


if __name__ == "__main__":
    main()
