"""Writes the reference answers of the Python site client for the Rust port's tests.

    python tools/golden_sites.py           # the parsing cases (no network)
    python tools/golden_sites.py --live    # also what the client fetches for an example creator

Output:
  desktop/src-tauri/tests/fixtures/sites.json — HTML/JSON parsing answers (committed)
  desktop/src-tauri/target/sites_live.json   — live results, for: cargo test --lib -- --ignored sites
The Rust client (src-tauri/src/downloader/sites.rs) must give the same answers.
"""

import json
import os
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core.api_client import ApiClient  # noqa: E402
from core.parser import LinkParser  # noqa: E402
from core.providers import normalize_cookie  # noqa: E402
from core.text_utils import clean_text  # noqa: E402

FIXTURES = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures")
TARGET = os.path.join(ROOT, "desktop", "src-tauri", "target")
CREATOR = "https://pawchive.pw/patreon/user/152819670"
POST = CREATOR + "/post/174018597"

TEXTS = [
    "", "  plain  ", "a\tb\r\nc   d", "Tom &amp; Jerry &lt;3 &#39;x&#39; &quot;q&quot;", "&nbsp;spaced&nbsp;",
    "cafÃ©", "itâ\u0080\u0099s", "itâ€™s", "ðŸ\u0090± cat", "Ã alone",
    "zero​width‏", "soft­hyphen", "bom﻿here", "ï»¿start", "replace�char",
    "emoji \U0001f408 stays", "日本語のテキスト", "line sep", "word⁠joiner", "&amp;amp;", "Ã©Ã©",
    "mixed cafÃ© and ☃", "ð only",
]

COOKIES = ["", "   ", "abc", "session=abc", "eyJhbGciOi.x.y", "eyJ.session=1", "a=b; c=d", "  token  ", "x=1"]

HTML_NAMES = [
    ('<span itemprop="name">Teshii</span>', "1"),
    ("<span itemprop='name'> Silly &amp; Teshii </span>", "1"),
    ('<span itemprop="name">152819670</span><title>Posts of Real Name from Patreon</title>', "152819670"),
    ('<div class="x user-header__name y">Header Name</div>', "1"),
    ("<title>  Posts of Someone from Patreon | Pawchive</title>", "1"),
    ("<title>Someone's posts</title>", "1"),
    ('<meta property="og:title" content="OG Name">', "1"),
    ('<meta property="og:title" content="Posts of nobody">', "1"),
    ('<meta property="og:title" content="1">', "1"),
    ("<html><body>nothing</body></html>", "1"),
    ("", "1"),
    ('<SPAN ITEMPROP="name">Upper</SPAN>', "1"),
    ('<span itemprop="name">\n  Multi\n  Line\n</span>', "1"),
]

TEMP_PAGES = [
    '<a href="https://t1.pawchive.pw/f/abc/My%20Video.mp4?e=123&amp;s=x">dl</a>'
    '<video src="https://t1.pawchive.pw/f/def/clip.mov?e=9"></video>'
    "<a href='https://t1.pawchive.pw/f/abc/My%20Video.mp4?e=999'>dup</a>",
    '<a href="https://example.com/f/x.mp4">no</a>',
    '<img src="https://t1.pawchive.pw/f/zz/%E6%97%A5%E6%9C%AC.png">',
    "",
]

future, past = int(time.time()) + 86400 * 365 * 5, int(time.time()) - 3600
TEMP_FILES = [
    {"path": f"https://t1.pawchive.pw/f/a/x.mp4?e={past}"},
    {"path": f"https://t1.pawchive.pw/f/a/x.mp4?e={future}"},
    {"temp_url": f"https://t1.pawchive.pw/f/a/x.mp4?e={past}", "path": "/data/x.mp4"},
    {"temp_url": f"https://t1.pawchive.pw/f/a/x.mp4?e={future}"},
    {"temp_expires": "2001-01-01T00:00:00Z"},
    {"temp_expires": "2099-01-01T00:00:00+00:00"},
    {"temp_expires": "garbage"},
    {"path": "/data/plain.mp4"},
    {},
]

BOOKMARKS = [
    ({"nodes": [{}, {}, {"data": [
        {"creators": 1, "total": 2}, [3, 8], 42,
        {"id": 4, "service": 5, "name": 6, "postCount": 7}, "123", "OnlyFans", "Some Creator", 77,
        {"id": "456", "service": "fansly", "displayName": "Display"},
    ]}]}, "artist"),
    ({"nodes": [{}, {}, {"data": [
        {"items": 1, "total": 9}, [2, 6], {"id": 3, "creatorId": 4, "service": 5, "extra": 3}, "p1", "c1", "Fansly",
        {"id": "p2", "user": "u2", "service": "onlyfans", "title": "Hello"},
    ]}]}, "post"),
    ({"nodes": [{}, {}]}, "post"),
    ([{"id": 1}], "artist"),
    ({"nodes": [{}, {}, {"data": []}]}, "artist"),
]

DM_PAGES = [
    """<html><body>
    <a href="/patreon/user/152819670/dms?o=50">next</a>
    <article class="dm-card" id="dm123"><div class="dm-card__content"><p>Hello there</p><p>second</p></div>
      <div class="dm-card__added">Added: 2024-03-05 10:00</div>
      <img src="/data/aa/pic.jpg"><img src="https://x/logo.png"><a href="//cdn.pawchive.pw/data/bb/file.zip?f=1">zip</a></article>
    <article class="dm-card"><div class="dm-card__content">   </div>
      <img src="relative/img.png"></article>
    </body></html>""",
    "<html><body><p>No DMs</p></body></html>",
]


def golden():
    c = ApiClient()
    parser = LinkParser()
    p = parser.parse(CREATOR)
    cases = []
    for t in TEXTS:
        cases.append({"fn": "clean_text", "args": {"text": t}, "result": clean_text(t)})
    for v in COOKIES:
        cases.append({"fn": "normalize_cookie", "args": {"v": v}, "result": normalize_cookie(v)})
    for html, uid in HTML_NAMES:
        cases.append({"fn": "creator_name_from_html", "args": {"html": html, "uid": uid}, "result": c._extract_creator_from_html(html, uid)})
    for html in TEMP_PAGES:
        cases.append({"fn": "temporary_links_from_html", "args": {"html": html},
                      "result": [[k, v] for k, v in ApiClient.extract_pawchive_temporary_attachments(html).items()]})
    for f in TEMP_FILES:
        cases.append({"fn": "temporary_link_expired", "args": {"f": f}, "result": ApiClient.is_pawchive_temp_expired(f)})
    for data, kind in BOOKMARKS:
        items, total = ApiClient._parse_cumst_bookmarks(data, kind)
        cases.append({"fn": "parse_cumst_bookmarks", "args": {"data": data, "kind": kind}, "result": [items, total]})
    for v in ["5", "0", "500", "-3", "abc", None, "Wed, 21 Oct 2015 07:28:00 GMT"]:
        cases.append({"fn": "retry_after", "args": {"v": v}, "result": __import__("core.api_client", fromlist=["x"])._retry_after_seconds(v, 7.0)})
    # The Pawchive DM page parser lives inside fetch_user_dms; feed it pages through a fake response
    for html in DM_PAGES:
        cases.append({"fn": "pawchive_dms_page", "args": {"html": html, "service": p.service, "user": p.user_id},
                      "result": dms_from_page(c, p, html)})
    os.makedirs(FIXTURES, exist_ok=True)
    with open(os.path.join(FIXTURES, "sites.json"), "w", encoding="utf-8") as fh:
        json.dump(cases, fh, ensure_ascii=False, indent=1)
    print(f"{len(cases)} cases -> tests/fixtures/sites.json")


class _Resp:
    def __init__(self, text):
        self.status_code, self.text, self.headers = 200, text, {}

    def json(self):
        return json.loads(self.text)


def dms_from_page(c, p, html):
    pages = [html]
    c._get_with_log = lambda *a, **k: _Resp(pages.pop(0)) if pages else _Resp("<html></html>")
    return c.fetch_user_dms(p, page_end=1)


def live():
    c = ApiClient()
    parser = LinkParser()
    p, post = parser.parse(CREATOR), parser.parse(POST)
    profile = c.fetch_creator_profile(p)
    single = c.fetch_single_post(post) or {}
    posts = c.fetch_user_posts(p, page_start=1, page_end=2)
    out = {
        "creator": CREATOR, "post": POST,
        "profile_name": profile.get("name"),
        "creator_name": c.resolve_creator_name(p),
        "single_post": single,
        "posts": [x.get("id") for x in posts],
        "posts_failed": c.last_fetch_failed(),
        "tags": c.fetch_creator_tags(p),
        "comments": c.fetch_post_comments(post.domain, post.service, post.user_id, post.post_id),
        "dms": c.fetch_user_dms(p, page_end=2),
        "listing_size": len(c.fetch_creator_updates("pawchive.pw") or {}),
    }
    os.makedirs(TARGET, exist_ok=True)
    with open(os.path.join(TARGET, "sites_live.json"), "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"live: {out['profile_name']!r}, {len(out['posts'])} posts, {len(out['tags'])} tags, "
          f"{len(out['comments'])} comments, {len(out['dms'])} DMs, {out['listing_size']} creators -> target/sites_live.json")


if __name__ == "__main__":
    golden()
    if "--live" in sys.argv:
        live()
