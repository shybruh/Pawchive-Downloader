"""Writes what the Python download archive (core/archive_manager.py) does when files and links are
recorded and looked up, for the Rust port's test (src-tauri/src/archive/mod.rs).

    .venv/Scripts/python tools/golden_archive.py

Output (desktop/src-tauri/tests/fixtures/):
  archive_empty.db — a new, empty archive as the Python archive creates it (tables, summary tables,
                     triggers, search index): the Rust test starts from a copy of it
  archive.json     — the operations with their answers, the tables afterwards (times left out:
                     they are "now") and what the search index finds
"""

import json
import os
import shutil
import sqlite3
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core import archive_index  # noqa: E402
from core.archive_manager import ArchiveManager  # noqa: E402

FIX = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures")
TIME_COLUMNS = {"downloaded_at", "last_verified_at", "newest", "oldest"}

F = lambda **kw: dict({"service": "patreon", "creator_id": "152819670", "creator_name": "SillyTeshii",  # noqa: E731
                       "post_id": "1", "post_title": "First", "file_id": "1_/ab/a.png", "file_hash": "",
                       "filename": "a.png", "file_size": 1000, "file_ext": "", "file_path": ""}, **kw)

OPS = [
    ("is_archived", {"service": "patreon", "post_id": "1", "file_id": "1_/ab/a.png", "file_hash": ""}),
    ("record_file", F(file_hash="aa" * 32, file_path="D:\\Art\\a.png")),
    ("is_archived", {"service": "PATREON", "post_id": "1", "file_id": "1_/ab/a.png", "file_hash": ""}),
    ("is_archived", {"service": "patreon", "post_id": "9", "file_id": "other", "file_hash": "aa" * 32}),
    ("is_archived", {"service": "patreon", "post_id": "9", "file_id": "other", "file_hash": "  "}),
    ("record_file", F(file_id="1_/cd/b.MP4", filename="Clip B.MP4", file_size=0)),
    ("record_file", F(service="Fanbox", creator_id="77", creator_name="", post_id="2", post_title="", file_id="2_x",
                      filename="", file_ext="ZIP", file_size=5)),
    # Recorded again: given values replace, empty ones keep what was there
    ("record_file", F(file_id="1_/cd/b.MP4", filename="", post_title="", file_size=0, file_hash="bb" * 32,
                      file_path="D:\\Art\\Clip B.mp4", creator_name="Silly T")),
    ("record_file", F(service="fanbox", creator_id="77", post_id="2", file_id="2_x", filename="pack.zip",
                      creator_name="Seventy", post_title="Pack")),
    ("record_link", {"service": "patreon", "creator_id": "152819670", "post_id": "1", "url": "  https://mega.nz/file/x#y ",
                     "creator_name": "SillyTeshii", "post_title": "First", "link_title": "[MEGA] https://mega.nz/file/x#y"}),
    ("record_link", {"service": "patreon", "creator_id": "152819670", "post_id": "3", "url": "https://gofile.io/d/AbC",
                     "creator_name": "", "post_title": "Third", "link_title": "  "}),
    ("record_link", {"service": "patreon", "creator_id": "1", "post_id": "3", "url": "   ", "creator_name": "",
                     "post_title": "", "link_title": ""}),
    ("record_file", F(post_id="4", file_id="4_ü", filename="Ünïcode ✨.png", post_title="Ärger 日本")),
    ("is_archived", {"service": "patreon", "post_id": "4", "file_id": "4_ü", "file_hash": ""}),
]
SEARCHES = ["Clip", "pack", "SillyTeshii", "日本語", "Ärger", "mega.nz", "152819670"]


def tables(path):
    c = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        out = {}
        names = [r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'archive_%' "
                                          "AND name NOT LIKE 'archive_fts%' ORDER BY name;")]
        for t in ["downloaded_files"] + names:
            cols = [r[1] for r in c.execute(f"PRAGMA table_info({t});") if r[1] not in TIME_COLUMNS]
            if not cols:
                continue
            rows = [list(r) for r in c.execute(f"SELECT {', '.join(cols)} FROM {t} ORDER BY {', '.join(cols)};")]
            out[t] = {"columns": cols, "rows": rows}
        # (as the Archive tab searches: core/archive_index.py FTS_MATCH)
        searches = {q: [r[0] for r in c.execute(f"SELECT rowid FROM archive_fts WHERE {archive_index.FTS_MATCH} ORDER BY rowid;",
                                                 archive_index.fts_query(q))] for q in SEARCHES}
        return out, searches
    finally:
        c.close()


def main():
    cfg = tempfile.mkdtemp(prefix="pawchive_tests_archive_")
    am = ArchiveManager(config_dir=cfg, enabled=True)
    am.checkpoint()
    am.close()
    db = os.path.join(cfg, "download_archive.db")
    for suffix in ("-wal", "-shm"):
        if os.path.exists(db + suffix) and os.path.getsize(db + suffix) == 0:
            os.remove(db + suffix)
    shutil.copy2(db, os.path.join(FIX, "archive_empty.db"))

    am = ArchiveManager(config_dir=cfg, enabled=True)
    ops = []
    for fn, args in OPS:
        ops.append({"fn": fn, "args": args, "result": getattr(am, fn)(**args)})
    am.checkpoint()
    am.close()
    t, s = tables(db)
    with open(os.path.join(FIX, "archive.json"), "w", encoding="utf-8") as fh:
        json.dump({"ops": ops, "tables": t, "searches": s}, fh, ensure_ascii=False, indent=1)
    print(f"{len(ops)} operations, tables {list(t)} -> tests/fixtures/archive.json, archive_empty.db "
          f"({os.path.getsize(os.path.join(FIX, 'archive_empty.db'))} bytes)")


if __name__ == "__main__":
    main()
