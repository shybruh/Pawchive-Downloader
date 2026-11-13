"""Writes the reference answers of the download history (core/history_store.py) and of the folder
a link's creator is added to the Watchlist with (bridge/app_bridge.py extract_artist_folder_from_path
and the queueing code's choice) for the Rust port's tests (src-tauri/src/history.rs,
src-tauri/src/downloader/scan.rs watchlist_folder).

    python tools/golden_history.py

History: sessions and downloaded files added (duplicates, empty ids, odd values), the History tab's
sessions read back with limits, an older version's history.json imported; the rows after each step.
Folders: a made-up download folder with creator folders under either name; files of a link in them.
Paths under the temporary folder are written as <B>.
Output: desktop/src-tauri/tests/fixtures/history.json
"""

import json
import os
import shutil
import sqlite3
import sys
import tempfile
from types import SimpleNamespace

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core import folder_naming  # noqa: E402
from core.history_store import HistoryStore, import_legacy_json  # noqa: E402
from bridge.app_bridge import AppBridge  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "history.json")


def rows(path):
    c = sqlite3.connect(path)
    try:
        return {"files": [list(r) for r in c.execute("SELECT id FROM files ORDER BY id")],
                "sessions": [list(r) for r in c.execute("SELECT seq, creator, url, service, files, date FROM sessions ORDER BY seq")]}
    finally:
        c.close()


def history_cases(base):
    db = os.path.join(base, "cfg", "history.db")
    st = HistoryStore(db)
    steps = []

    def step(op, args, result):
        steps.append({"op": op, "args": args, "result": result, "rows": rows(db) if os.path.exists(db) else None})

    step("recent", {"limit": 500}, st.recent_sessions())
    step("file_count", {}, st.file_count())
    for args in [{"ids": ["f1", "f2", "f1", "", "f3"]}, {"ids": []}, {"ids": ["f2", "日本語"]}]:
        st.add_files(args["ids"])
        step("add_files", args, None)
    for args in [{"entries": [{"creator": "Alice", "url": "https://x/a", "service": "patreon", "files": 12, "date": "2026-11-01 10:00"}]},
                 {"entries": [{"creator": "Bob", "url": "", "service": "", "files": "7", "date": ""},
                              {"creator": "Céline", "url": "https://x/c", "service": "fanbox", "files": None, "date": "2026-11-02 09:30"}]},
                 {"entries": [{}]}]:
        st.add_sessions(args["entries"])
        step("add_sessions", args, None)
    step("recent", {"limit": 500}, st.recent_sessions())
    step("recent", {"limit": 2}, st.recent_sessions(2))
    step("file_count", {}, st.file_count())
    legacy = {"downloaded_files": ["old1", "f1", 5, None, "old2"],
              "download_history": [{"creator": "New", "url": "u2", "service": "s", "files": 3, "date": "2025-02-02 00:00"},
                                   {"creator": "Old", "url": "u1", "service": "s", "files": 1, "date": "2025-01-01 00:00"},
                                   "not a session"]}
    n = import_legacy_json(st, legacy)
    step("import_legacy", {"data": legacy}, n)
    step("recent", {"limit": 3}, st.recent_sessions(3))
    return steps


def folder_cases(base):
    dl = os.path.join(base, "dl")
    for d in ["Alice [patreon]", "bob", "Carol [fanbox]/2026 post/sub", "Dave", "dave [fantia]", "Eve [x]/a", "deep/nest/Frank [fanbox]/p"]:
        os.makedirs(os.path.join(dl, d.replace("/", os.sep)), exist_ok=True)
    fake = SimpleNamespace(_download_dir=os.path.join(base, "default"))
    cases = []
    for name, service, artist_dir, first in [
        ("Alice", "patreon", "", os.path.join(dl, "Alice [patreon]", "post 1", "a.jpg")),
        ("Alice", "patreon", os.path.join(base, "chosen"), os.path.join(dl, "x.jpg")),
        ("bob", "", "", os.path.join(dl, "bob", "p", "b.jpg")),
        ("Carol", "fanbox", "", os.path.join(dl, "Carol [fanbox]", "2026 post", "sub", "c.jpg")),
        ("carol", "fanbox", "", os.path.join(dl, "CAROL [FANBOX]", "c.jpg")),
        ("Dave", "fantia", "", os.path.join(dl, "elsewhere", "d.jpg")),
        ("Eve", "x", "", os.path.join(dl, "Eve [x]", "a", "e.jpg")),
        ("Eve", "patreon", "", os.path.join(dl, "Eve [x]", "a", "e.jpg")),
        ("Frank", "fanbox", "", os.path.join(dl, "deep", "nest", "Frank [fanbox]", "p", "f.jpg")),
        ("Ghost/Name: bad*", "patreon", "", os.path.join(dl, "Ghost_Name_ bad [patreon]", "g.jpg")),
        ("Nobody", "patreon", "", ""),
        ("", "patreon", "", os.path.join(dl, "z.jpg")),
        ("Dave", "fantia", "", ""),
    ]:
        for include in (True, False):
            folder_naming.include_service = include
            got = (artist_dir or (AppBridge.extract_artist_folder_from_path(fake, first, name or "uid1", service, fallback_dir=dl)
                                  if first else folder_naming.pick_creator_folder(dl, name or "uid1", service)))
            cases.append({"creator_name": name, "user_id": "uid1", "service": service, "artist_dir": artist_dir,
                          "first_target": first, "download_dir": dl, "include_service": include, "folder": got})
    folder_naming.include_service = True
    return cases


def main():
    base = tempfile.mkdtemp(prefix="pawchive_history_")
    try:
        B = os.path.realpath(base)
        out = {"history": history_cases(B), "folders": folder_cases(B)}
        text = json.dumps(out, ensure_ascii=False, indent=1).replace(json.dumps(B)[1:-1], "<B>")
    finally:
        shutil.rmtree(base, ignore_errors=True)
    with open(FIXTURE, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print(f"wrote {FIXTURE}: {len(out['history'])} history steps, {len(out['folders'])} folder cases")


if __name__ == "__main__":
    main()
