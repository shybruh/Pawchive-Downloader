"""Writes what the Python watchlist store (core/watchlist_store.py) does with a fixed series of
operations, for the Rust port's test (src-tauri/src/watchlist/store.rs).

    python tools/golden_watchlist_store.py

Output: desktop/src-tauri/tests/fixtures/watchlist_store.json — the operations, each one's answer,
and the database's tables (every column of every row) after each of them. The Rust store must
leave the same database.
"""

import json
import os
import sqlite3
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core.watchlist_store import WatchlistStore  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "watchlist_store.json")


def fields(uid, name, **kw):
    d = {"url": f"https://pawchive.pw/patreon/user/{uid}", "service": "patreon", "domain": "pawchive.pw",
         "user_id": uid, "creator_name": name, "last_post_id": "", "last_post_date": "", "added_at": "2026-11-01T10:00:00",
         "auto_check": True, "download_dir": "D:\\Art\\" + name, "download_dirs": ["D:\\Art\\" + name],
         "ignored_post_ids": [], "cutoff_day_ids": [], "is_favorite": False}
    d.update(kw)
    return d


OPTS_A = {"file_type": "all", "subfolder_per_post": True, "skip_words": "wip, sketch", "max_size": 0}
OPTS_B = {"file_type": "images", "date_after": "2024-01-01", "ratio": 1.5}
POSTS = [{"id": "101", "title": "Ünïcödé ✨ post", "published": "2026-11-02T08:00:00",
          "attachments": [{"name": "a.png", "path": "/ab/cd/abcd.png"}]},
         {"id": "102", "title": "Second \"quoted\" \\ back", "added": "2026-11-03"}]

OPS = [
    ("has_rows", {}),
    ("write", {"rows": [[1, 0, fields("1", "Alice"), OPTS_A, []],
                        [2, 1, fields("2", "Bob", last_post_date="2026-10-01", last_post_id="99", cutoff_day_ids=["99"]), OPTS_A, POSTS],
                        [3, 2, fields("3", "Cara", is_favorite=True, last_checked_at=1794082361.9700677, custom_folder=True), {}, []]],
               "deleted": [], "replace_all": False}),
    ("has_rows", {}),
    ("load", {}),
    ("write", {"rows": [[2, 1, fields("2", "Bob", last_post_date="2026-11-03", last_post_id="102"), OPTS_B, []],
                        [4, 3, fields("4", "Đặng 日本", service="fanbox"), OPTS_B, POSTS[:1]]],
               "deleted": [1], "replace_all": False}),
    ("load", {}),
    ("set_meta", {"key": "version", "value": "1"}),
    ("get_meta", {"key": "version", "default": ""}),
    ("get_meta", {"key": "missing", "default": "fallback"}),
    ("drop_unused_options", {}),
    ("write", {"rows": [[7, 0, fields("7", "Zed"), OPTS_A, []], [8, 1, fields("8", "Yan"), {}, []]],
               "deleted": [], "replace_all": True}),
    ("load", {}),
    ("drop_unused_options", {}),
    ("write", {"rows": [], "deleted": [8, 99], "replace_all": False}),
    ("load", {}),
]


def tables(path):
    if not os.path.exists(path):
        return {}
    c = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return {t: [list(r) for r in c.execute(f"SELECT * FROM {t} ORDER BY 1;")] for t in ("creators", "options", "meta")}
    finally:
        c.close()


def main():
    path = os.path.join(tempfile.mkdtemp(prefix="pawchive_tests_wlstore_"), "watchlist.db")
    s = WatchlistStore(path)
    out = []
    for op, args in OPS:
        if op == "write":
            r = s.write([tuple(x) for x in args["rows"]], args["deleted"], args["replace_all"])
        elif op == "load":
            r = [list(x) for x in s.load()]
        else:
            r = getattr(s, op)(**args)
        out.append({"op": op, "args": args, "result": r, "tables": tables(path)})
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"{len(out)} operations -> tests/fixtures/watchlist_store.json")


if __name__ == "__main__":
    main()
