"""Writes what the Python watchlist model (bridge/watchlist_model.py) shows for a made-up watchlist,
for the Rust port's test (src-tauri/src/watchlist/view.rs).

    .venv/Scripts/python tools/golden_watchlist_view.py

Output: desktop/src-tauri/tests/fixtures/watchlist_view.json — the artists (as the app sends them),
the folders that exist (in a download folder written as {BASE}), and for each search / category:
the rows in order, the counts and the first row's details.
"""

import json
import os
import shutil
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core import folder_naming  # noqa: E402
from core.watchlist_manager import WatchlistEntry  # noqa: E402
from bridge.watchlist_model import WatchlistModel  # noqa: E402
from services.rust_watchlist_view import row_of  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "watchlist_view.json")
BASE = os.path.join(ROOT, "desktop", "src-tauri", "target", "wlview_base")

DIRS = ["Alice [patreon]", "bob", "Own Folder", "Deep\\Nested"]

POSTS = [
    {"id": "11", "title": "  Spaced title  ", "published": "2026-11-02T08:00:00", "content": "<p>long</p>",
     "file": {"name": "a.png", "path": "/aa/a.png"}, "attachments": [{"name": "a.png", "path": "/AA/A.PNG"}, {"path": "/bb/b.png"}]},
    {"id": 12, "title": "", "captionText": "From caption", "added": "2026-11-03", "file": {"name": "only.png"}},
    {"id": "13", "caption": "Cap", "published": 1782600000, "attachments": [{"storageKey": "k1"}, {"storageKey": "k1"}, {"name": "x"}]},
    {"id": None, "file": {}, "attachments": []},
    {"title": "No id", "file": {"path": "/cc/c.png"}, "attachments": [{"path": "/dd/d.png"}]},
]


def E(name, uid, service="patreon", domain="pawchive.pw", **kw):
    e = WatchlistEntry(url=f"https://{domain}/{service}/user/{uid}", service=service, domain=domain, user_id=uid,
                       creator_name=name, added_at="2026-10-01T00:00:00")
    for k, v in kw.items():
        setattr(e, k, v)
    return e


def entries():
    return [
        E("Alice", "1", new_post_count=3, cached_new_posts=POSTS[:3], is_favorite=True, last_post_date="2026-11-01",
          last_post_id="10"),
        E("bob", "2", service="fanbox", auto_check=False, ignored_post_ids=["1", "2"]),
        E("Carol", "3", download_dir=os.path.join(BASE, "Own Folder"), download_dirs=[os.path.join(BASE, "Own Folder"), "Z:\\gone"]),
        E("dave", "4", download_dirs=["Z:\\missing", os.path.join(BASE, "Deep", "Nested")], new_post_count=1,
          cached_new_posts=POSTS[3:]),
        E("", "5555", service="onlyfans", domain="cum.st", is_favorite=True),
        E("Eve", "6", domain="kemono.su", new_post_count=2, cached_new_posts=POSTS[1:2]),
        E("alice", "7", service="fanbox", download_dir="Z:\\nope"),
        E("Ünïcode Ärtist", "8"),
    ]


def snapshot(es, search, filt):
    display, n_of, on_disk_of, (total, updated, new_posts, _) = WatchlistModel._compute((es, search, filt, BASE))
    roles = {int(k): bytes(v).decode() for k, v in WatchlistModel.roleNames(WatchlistModel).items()}

    def disk(e):
        return bool(on_disk_of[id(e)]) if id(e) in on_disk_of else WatchlistModel._is_entry_on_disk(e, BASE)

    rows = []
    for e in display:
        r = {}
        for role, name in roles.items():
            r[name] = disk(e) if name == "isOnDisk" else WatchlistModel._ROLE_GETTERS[role](e)
        rows.append(r)
    subdirs = {d.name.lower() for d in os.scandir(BASE) if d.is_dir()}
    on_disk = sum(1 for e in es if WatchlistModel._is_entry_on_disk(e, BASE, subdirs, {}))
    get0 = {}
    if display:
        e = display[0]
        from bridge.watchlist_model import _download_dirs, _posts_for_qml, _provider
        get0 = {"url": e.url, "creatorName": e.creator_name, "service": e.service, "domain": e.domain,
                "userId": e.user_id, "lastPostDate": e.last_post_date, "lastPostId": e.last_post_id,
                "addedAt": e.added_at, "autoCheck": e.auto_check, "newPostCount": e.new_post_count,
                "downloadDir": e.download_dir, "downloadDirs": _download_dirs(e),
                "ignoredCount": len(e.ignored_post_ids or []), "cachedNewPosts": _posts_for_qml(e),
                "isFavorite": bool(e.is_favorite), "provider": _provider(e), "isOnDisk": disk(e)}
    counts = [len(display), total, updated, new_posts, sum(1 for e in es if e.auto_check),
              sum(1 for e in es if e.is_favorite), on_disk]
    return {"rows": rows, "counts": counts, "get0": get0}


def main():
    shutil.rmtree(BASE, ignore_errors=True)
    for d in DIRS:
        os.makedirs(os.path.join(BASE, d))
    folder_naming.include_service = True
    es = entries()
    cases = []
    for search, filt in [("", "all"), ("", "updates"), ("", "favorites"), ("", "synced"), ("", "on_disk"), ("", "fanbox"),
                         ("", "PATREON"), ("ali", "all"), ("55", "all"), ("ALICE", "updates"), ("zzz", "all"), ("  ", ""),
                         ("e", "ondisk"), ("", "favorite")]:
        cases.append({"search": search, "filter": filt, "want": snapshot(es, search, filt)})
    sub = lambda v: json.loads(json.dumps(v).replace(json.dumps(BASE)[1:-1], "{BASE}"))  # noqa: E731
    out = {"dirs": DIRS, "include_service": True, "rows": sub([row_of(e) for e in es]), "cases": sub(cases)}
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    shutil.rmtree(BASE, ignore_errors=True)
    print(f"{len(cases)} cases -> tests/fixtures/watchlist_view.json")


if __name__ == "__main__":
    main()
