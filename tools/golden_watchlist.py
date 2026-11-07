"""Writes the reference answers of the watchlist's new-post check (WatchlistManager.get_posts_since in
core/watchlist_manager.py) for the Rust port's test (src-tauri/src/watchlist/mod.rs).

    python tools/golden_watchlist.py

Output: desktop/src-tauri/tests/fixtures/watchlist.json

A stand-in site client serves made-up post lists page by page (some pages can fail), and creators
with different "last download" points are checked against them: which posts come back as new,
in which order, and which pages were asked for.
"""

import datetime
import json
import os
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core.watchlist_manager import WatchlistEntry, WatchlistManager  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "watchlist.json")


class Client:
    def __init__(self, pages, fail_pages=()):
        self.pages, self.fail, self.asked, self._failed = pages, set(fail_pages), [], False

    def fetch_user_posts(self, parsed, page_start=1, page_end=1, page_size=50, cancel_event=None):
        self.asked.append(page_start)
        self._failed = page_start in self.fail
        if self._failed:
            return []
        return list(self.pages[page_start - 1]) if page_start <= len(self.pages) else []

    def last_fetch_failed(self):
        return self._failed


def day(n):
    return (datetime.date(2026, 6, 30) - datetime.timedelta(days=n)).isoformat()


def numeric_pages(n_pages=3, per=50, per_day=3, last=37):
    """Newest first; ids grow with time; a few posts a day"""
    posts = []
    pid = 900000
    for i in range(n_pages * per - (per - last)):
        d = day(i // per_day)
        when = f"{d}T{10 + (i % per_day):02d}:00:00" if i % 2 else d
        posts.append({"id": str(pid - i * 7), "title": f"p{i}", "published": when})
    return [posts[i:i + per] for i in range(0, len(posts), per)]


def hex_pages():
    posts = []
    for i in range(40):
        posts.append({"id": f"{(0xabc123 * (i + 3)) & 0xffffff:06x}", "title": f"h{i}", "published": day(i // 4)})
    return [posts[i:i + 20] for i in range(0, len(posts), 20)]


def mixed_pages():
    page = [
        {"id": "50", "published": 1782600000},                 # a timestamp (local time)
        {"id": "49", "published": None, "added": day(3) + "T08:00:00"},
        {"id": "48", "published": "", "added": day(4)},
        {"id": "47", "published": day(4) + "T23:59:59.123"},
        {"id": "46", "published": day(5)},
        {"id": "45"},                                           # no date at all
        {"id": 44, "published": day(6)},                        # a numeric id
    ]
    return [page]


NUM = numeric_pages()
HEX = hex_pages()
MIX = mixed_pages()


def E(**kw):
    base = dict(url="https://pawchive.pw/patreon/user/1", service="patreon", domain="pawchive.pw", user_id="1",
                creator_name="Someone")
    base.update(kw)
    return WatchlistEntry(**base)


CASES = [
    ("never downloaded", NUM, E(), 1, 999999, False, []),
    ("never downloaded, pages 2-2", NUM, E(), 2, 2, False, []),
    ("cutoff mid page 2 with id", NUM, E(last_post_date=NUM[1][20]["published"][:10], last_post_id=NUM[1][20]["id"]), 1, 999999, False, []),
    ("cutoff date only", NUM, E(last_post_date=day(20)), 1, 999999, False, []),
    ("cutoff with ISO date", NUM, E(last_post_date=day(5) + "T12:00:00", last_post_id=NUM[0][16]["id"]), 1, 999999, False, []),
    ("cutoff day ids", NUM, E(last_post_date=day(2), last_post_id="", cutoff_day_ids=[NUM[0][7]["id"]]), 1, 999999, False, []),
    ("up to date", NUM, E(last_post_date=day(0), last_post_id=NUM[0][0]["id"]), 1, 999999, False, []),
    ("in the future", NUM, E(last_post_date="2027-01-01"), 1, 999999, False, []),
    ("ignored posts", NUM, E(last_post_date=day(3), last_post_id=NUM[0][10]["id"],
                             ignored_post_ids=[NUM[0][0]["id"], NUM[0][4]["id"], "nope"]), 1, 999999, False, []),
    ("very old cutoff, last page partial", NUM, E(last_post_date="2020-01-01"), 1, 999999, False, []),
    ("page end 1", NUM, E(last_post_date="2020-01-01"), 1, 1, False, []),
    ("page start after end", NUM, E(), 3, 1, False, []),
    ("page start 0", NUM, E(last_post_date=day(25)), 0, 999999, False, []),
    ("page 2 fails, raise", NUM, E(last_post_date="2020-01-01"), 1, 999999, True, [2]),
    ("page 2 fails, no raise", NUM, E(last_post_date="2020-01-01"), 1, 999999, False, [2]),
    ("hex ids with day ids", HEX, E(last_post_date=day(3), last_post_id=HEX[0][13]["id"],
                                    cutoff_day_ids=[HEX[0][12]["id"], HEX[0][13]["id"]]), 1, 999999, False, []),
    ("hex ids, id only", HEX, E(last_post_date=day(3), last_post_id=HEX[0][13]["id"]), 1, 999999, False, []),
    ("hex ids, never", HEX, E(), 1, 999999, False, []),
    ("timestamps and added", MIX, E(last_post_date=day(5)), 1, 999999, False, []),
    ("timestamps, never", MIX, E(), 1, 999999, False, []),
    ("timestamps, cutoff id", MIX, E(last_post_date=day(4), last_post_id="47"), 1, 999999, False, []),
    ("empty list", [], E(last_post_date=day(1)), 1, 999999, False, []),
]


def main():
    m = WatchlistManager(tempfile.mkdtemp(prefix="pawchive_tests_watch_"))
    out = []
    for name, pages, entry, ps, pe, raise_on_error, fails in CASES:
        c = Client(pages, fails)
        try:
            got = m.get_posts_since(entry, c, raise_on_error=raise_on_error, page_start=ps, page_end=pe)
            want = {"ids": [p["id"] for p in got], "pages": c.asked}
        except RuntimeError as e:
            want = {"raised": str(e), "pages": c.asked}
        fields = entry.store_fields()
        out.append({"name": name, "pages": pages, "fail_pages": fails, "page_start": ps, "page_end": pe,
                    "raise_on_error": raise_on_error, "want": want,
                    "entry": {k: fields[k] for k in ("creator_name", "last_post_date", "last_post_id", "cutoff_day_ids",
                                                     "ignored_post_ids")}})
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"{len(out)} cases -> tests/fixtures/watchlist.json")
    for o in out:
        print(f"  {o['name']}: {len(o['want'].get('ids', []))} new, pages {o['want']['pages']}"
              + (f" raised: {o['want']['raised']}" if "raised" in o["want"] else ""))


if __name__ == "__main__":
    main()
