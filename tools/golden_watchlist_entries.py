"""Writes the reference answers of the watchlist's operations (WatchlistManager in
core/watchlist_manager.py: adding, favorites, removing, the "downloaded up to" point, marking
updates done or ignored, auto-check, download folders) for the Rust port's test
(src-tauri/src/watchlist/entries.rs).

    python tools/golden_watchlist_entries.py

Output: desktop/src-tauri/tests/fixtures/watchlist_entries.json

A watchlist of a few creators, then a list of operations: after each one, its answer and every
creator's fields; at the end, what the database holds. Folders are made under a scratch folder
({BASE} in the fixture), where adding a creator again renames its "<id> [service]" folder.
"""

import json
import os
import shutil
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
os.environ["PAWCHIVE_PYTHON_WATCHLIST"] = "1"

import core.watchlist_manager as wm  # noqa: E402
from core import folder_naming  # noqa: E402
from core.watchlist_store import WatchlistStore  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "watchlist_entries.json")
BASE = os.path.join(ROOT, "desktop", "src-tauri", "target", "watchlist_entries")


def post(pid, published=None, added=None, **kw):
    p = {"id": pid, "title": f"Post {pid}", "published": published, "added": added, "content": "<p>long text</p>",
         "file": {"name": f"{pid}.png", "path": f"/aa/{pid}.png", "extra": 1}, "attachments": [
             {"name": "a.zip", "path": f"/bb/{pid}.zip"}, {"name": "nothing"}]}
    p.update(kw)
    return p


START = [
    dict(url="https://pawchive.pw/patreon/user/111", service="patreon", domain="pawchive.pw", user_id="111",
         creator_name="Alpha", last_post_id="500", last_post_date="2026-01-10", added_at="2025-01-01T00:00:00",
         download_dir="{BASE}\\Alpha [patreon]", download_dirs=["{BASE}\\Alpha [patreon]", "{BASE}\\old\\Alpha"],
         cutoff_day_ids=["500"], options={"file_type": "images"}, last_checked_at=1794082361.9700677),
    dict(url="https://pawchive.pw/fanbox/user/222", service="fanbox", domain="pawchive.pw", user_id="222",
         creator_name="222", added_at="2025-02-01T00:00:00", download_dir="{BASE}\\222 [fanbox]", auto_check=False),
    dict(url="https://pawchive.pw/boosty/user/bee", service="boosty", domain="pawchive.pw", user_id="bee",
         creator_name="Bee", last_post_id="abc-1", last_post_date="2026-03-01T12:00:00", added_at="2025-03-01T00:00:00",
         ignored_post_ids=["x1"], custom_folder=True, is_favorite=True),
]
CACHED = {
    "111": [post("501", "2026-01-10T05:00:00"), post("499", "2026-01-10"), post("600", "2026-01-12T00:00:00"),
            post("601", None, "2026-01-13"), post(700, "2026-02-01")],
    "bee": [post("abc-2", "2026-03-01T13:00:00"), post("abc-3", "2026-03-02"), post("abc-4", "2026-03-01")],
}

OPS = [
    ("update_last_download", dict(user_id="111", service="patreon", post_id="501", post_date="2026-01-10T05:00:00")),
    ("update_last_download", dict(user_id="111", service="patreon", post_id="600", post_date="2026-01-12", day_ids=["600", "599"])),
    ("update_last_download", dict(user_id="nobody", service="patreon", post_id="1", post_date="2026-01-01")),
    ("update_last_download", dict(user_id="BEE ", service="Boosty", post_id="abc-2", post_date="2026-03-01")),
    ("ignore_post", dict(user_id="111", service="patreon", post_id="601")),
    ("ignore_post", dict(user_id="111", service="patreon", post_id="601")),
    ("ignore_post", dict(user_id="111", service="patreon", post_id="  ")),
    ("ignore_post", dict(user_id="111", service="patreon", post_id="700")),
    ("unignore_post", dict(user_id="111", service="patreon", post_id="601")),
    ("unignore_post", dict(user_id="111", service="patreon", post_id="nope")),
    ("unignore_all", dict(user_id="bee", service="boosty")),
    ("unignore_all", dict(user_id="bee", service="boosty")),
    ("resolve_posts", dict(user_id="bee", service="boosty", post_ids=["abc-3"])),
    ("resolve_posts", dict(user_id="bee", service="boosty", post_ids=["abc-4"], latest_post_id="abc-4",
                           latest_post_date="2026-03-02", day_ids=["abc-4", "abc-5"])),
    ("resolve_posts", dict(user_id="111", service="patreon", latest_post_id="800", latest_post_date="2026-02-02")),
    ("resolve_posts", dict(user_id="zzz", service="patreon")),
    ("set_auto_check", dict(user_id="111", service="patreon", enabled=False)),
    ("set_all_auto_check", dict(enabled=True)),
    ("set_all_auto_check", dict(enabled=True)),
    ("set_download_dir", dict(user_id="bee", service="boosty", download_dir="{BASE}/bee/../Bee")),
    ("set_download_dir", dict(user_id="bee", service="boosty", download_dir="{BASE}\\Bee")),
    ("add_download_dir", dict(user_id="bee", service="boosty", new_dir="{BASE}\\more\\Bee")),
    ("add_download_dir", dict(user_id="222", service="fanbox", new_dir="")),
    ("remove_download_dir", dict(user_id="111", service="patreon", target_dir="{BASE}\\ALPHA [patreon]")),
    ("remove_download_dir", dict(user_id="111", service="patreon", target_dir="{BASE}\\old\\Alpha")),
    ("set_download_dir", dict(user_id="111", service="patreon", download_dir="")),
    ("set_favorite", dict(user_id="111", service="patreon", is_favorite=True)),
    ("set_favorite", dict(user_id="111", service="patreon", is_favorite=True)),
    ("set_favorite", dict(user_id="nope", service="patreon", is_favorite=True)),
    ("add_entry", dict(url="https://pawchive.pw/fanbox/user/222", creator_name="Bravo Two", user_id="222", service="fanbox",
                       domain="pawchive.pw", last_post_id="10", last_post_date="2026-05-05")),
    ("add_or_update_favorite_creator", dict(user_id="222", service="fanbox", creator_name="Bravo", domain="cum.st")),
    ("add_or_update_favorite_creator", dict(user_id="333", service="onlyfans", creator_name="Charlie", domain="cum.st")),
    ("add_or_update_favorite_creator", dict(user_id="444", service="patreon", creator_name="", domain="pawchive.pw")),
    ("add_or_update_favorite_creator", dict(user_id="444", service="patreon", creator_name="Delta", url="https://x/y", is_favorite=False)),
    ("add_entry", dict(url="", creator_name="Echo", user_id="555", service="patreon", domain="pawchive.pw",
                       last_post_id="42", last_post_date="2026-04-04", auto_check=False, download_dir="{BASE}\\Echo",
                       options={"skip_zip": True})),
    ("add_entry", dict(url="https://pawchive.pw/patreon/user/111", creator_name="111", user_id="111", service="patreon",
                       domain="pawchive.pw", last_post_id="", last_post_date="2026-01-01", download_dir="{BASE}\\elsewhere",
                       options={"file_type": "videos"}, is_favorite=True)),
    ("add_entry", dict(url="", creator_name="Foxtrot", user_id="666", service="gumroad", domain="pawchive.pw")),
    ("add_entry", dict(url="", creator_name="Foxtrot Renamed", user_id="666", service="gumroad", domain="pawchive.pw",
                       download_dir="{BASE}\\Fox")),
    ("set_last_post_date", dict(user_id="111", service="patreon", raw_date_str="2025-12-31")),
    ("set_last_post_date", dict(user_id="bee", service="boosty", raw_date_str="never")),
    ("set_last_post_date", dict(user_id="nope", service="boosty", raw_date_str="2025-01-01")),
    ("remove_entry", dict(user_id="333", service="onlyfans")),
    ("remove_entry", dict(user_id="333", service="onlyfans")),
    ("update_last_download", dict(user_id="555", service="patreon", post_id="41", post_date="2026-04-04")),
    ("update_last_download", dict(user_id="555", service="patreon", post_id="", post_date="")),
    ("update_last_download", dict(user_id="666", service="gumroad", post_id="9", post_date="")),
]


def sub(v):
    if isinstance(v, str):
        return v.replace(BASE, "{BASE}")
    if isinstance(v, list):
        return [sub(x) for x in v]
    if isinstance(v, dict):
        return {k: sub(x) for k, x in v.items()}
    return v


def unsub(v):
    if isinstance(v, str):
        return v.replace("{BASE}", BASE)
    if isinstance(v, list):
        return [unsub(x) for x in v]
    if isinstance(v, dict):
        return {k: unsub(x) for k, x in v.items()}
    return v


def snapshot(m):
    out = []
    for e in m.entries:
        f = wm._full(e)
        if f["added_at"] >= "2026-01-01":    # (made now)
            f["added_at"] = "<now>"
        out.append({"rid": e._rid, "f": sub(f)})
    return out


def main():
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(os.path.join(BASE, "222 [fanbox]"))
    folder_naming.include_service = True
    m = wm.WatchlistManager(os.path.join(BASE, "config"))
    m.store = WatchlistStore(m.db_file)
    for d in START:
        e = wm.WatchlistEntry.from_dict(unsub(d))
        e.cached_new_posts = [dict(p) for p in CACHED.get(e.user_id, [])]
        e.new_post_count = len(e.cached_new_posts)
        m.entries.append(e)
    m.save(full=True)
    start = [dict(c, seq=e._seq) for c, e in zip(snapshot(m), m.entries)]
    steps = []
    for op, args in OPS:
        r = getattr(m, op)(**unsub(args))
        if isinstance(r, wm.WatchlistEntry):
            r = r._rid
        if isinstance(r, tuple):
            r = list(r)
        steps.append({"op": op, "args": args, "result": sub(r), "entries": snapshot(m)})
    m.flush()
    db = sub([[rid, pos, d, posts] for rid, pos, d, posts in m.store.load()])
    for row in db:
        if row[2].get("added_at", "") >= "2026-01-01":
            row[2]["added_at"] = "<now>"
    folders = sorted(os.listdir(BASE))
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump({"start": start, "steps": steps, "db": db, "folders": folders}, fh, ensure_ascii=False, indent=1)
    shutil.rmtree(BASE, ignore_errors=True)
    print(f"-> {os.path.relpath(FIXTURE, ROOT)} ({len(steps)} operations, {len(start)} creators at the start)")


if __name__ == "__main__":
    main()
