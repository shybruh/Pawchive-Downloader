"""Writes the reference answers of the Python link parser for the Rust port's test.

    python tools/golden_links.py

Output: desktop/src-tauri/tests/fixtures/links.json — one entry per link with what
core.parser.LinkParser.parse() returns. The Rust parser (src-tauri/src/downloader/links.rs) must
give the same answer for every one.
"""

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core.parser import LinkParser  # noqa: E402

SERVICES = ["patreon", "fanbox", "fantia", "onlyfans", "fansly", "subscribestar", "boosty", "gumroad", "candfans", "discord"]

LINKS = [
    "", "   ", "not a link", "https://", "https://example.com", "https://example.com/patreon/user/1",
    # Archive sites: creators, posts, DMs, odd spellings
    "https://pawchive.pw/patreon/user/152819670",
    "https://pawchive.pw/patreon/user/152819670/",
    "https://pawchive.pw/patreon/user/152819670/post/174018597",
    "https://pawchive.pw/patreon/user/152819670/post/174018597/",
    "https://pawchive.pw/patreon/user/152819670/post/174018597?foo=bar#x",
    "https://pawchive.pw/patreon/user/152819670/posts",
    "https://pawchive.pw/patreon/user/152819670/dms",
    "https://pawchive.pw/patreon/user/152819670/dm",
    "https://pawchive.pw/patreon/user/152819670?tab=dms",
    "https://pawchive.pw/patreon/user/152819670?o=50&tab=dm",
    "https://pawchive.pw/patreon/user/152819670?tab=dmsx",
    "https://pawchive.pw/Patreon/User/ABC",
    "https://www.pawchive.pw/fanbox/user/3316400",
    "http://pawchive.pw/fantia/user/12345/post/999",
    "pawchive.pw/patreon/user/1",
    "PAWCHIVE.PW/PATREON/USER/1/POST/2",
    "  https://pawchive.pw/patreon/user/1  ",
    "https://img.pawchive.pw/patreon/user/1",
    "https://pawchive.pw/creators/patreon/152819670",
    "https://pawchive.pw/artists/fanbox/42/post/7",
    "https://pawchive.pw/posts/patreon/152819670/174018597",
    "https://pawchive.pw/discord/server/123",
    "https://cum.st/onlyfans/user/belledelphine",
    "https://cum.st/onlyfans/user/belledelphine/post/123456",
    "https://cum.st/fansly/user/9988/dms",
    "https://www.cum.st/candfans/user/x?tab=dms",
    "https://kemono.cr/patreon/user/1",
    "https://kemono.su/patreon/user/1/post/2",
    "https://coomer.st/onlyfans/user/abc",
    "https://coomer.su/onlyfans/user/abc/post/9",
    "https://example.org/patreon/user/1/post/2",
    "https://pawchive.pw.evil.com/patreon/user/1",
    # Telegram
    "https://t.me/c/1234567890/45", "https://telegram.me/c/1/2", "https://t.me/+AbCdEf_123", "https://t.me/joinchat/XyZ-9",
    "https://t.me/somechannel/123", "https://t.me/s/somechannel/123", "https://t.me/somechannel", "https://t.me/somechannel/",
    "https://t.me/abc", "https://t.me/s/channel_name", "https://www.t.me/chan_nel/7", "t.me/channelname",
    "https://t.me/channelname?single", "https://t.me/channelname/123?comment=4",
    # Bunkr and friends
    "https://bunkr.si/a/AbCd1234", "https://bunkr.cr/v/xyz_9", "https://bunkrr.ru/d/file-1", "https://cdn9.bunkr.ru/f/Q",
    "https://bunk.la/i/img", "https://bunkr.black/file/abc", "https://bunkr.si/x/nope",
    "https://balbums.st/a/album1", "https://balbums.st/", "https://www.balbums.st/a/q-1",
    "https://c.cdn.cr/storage/media/abcdef.mp4", "https://x.scdn.st/storage/media/file123", "https://scdn.st/storage/media/a.jpg",
    "https://www.erome.com/a/AbC123", "https://erome.com/a/x", "https://erome.com/user",
    "https://nhentai.net/g/123456/", "https://www.nhentai.net/g/1", "https://nhentai.net/tag/x",
    "https://saint2.su/embed/abc",
    # Native sites (cross-posts)
    "https://www.patreon.com/posts/some-title-123456789", "https://patreon.com/posts/987", "https://patreon.com/user?u=1",
    "https://www.fanbox.cc/@creator/posts/123", "https://creator.fanbox.cc/posts/456",
    "https://fantia.jp/posts/789", "https://boosty.to/someone/posts/abc-123", "https://subscribestar.adult/posts/42",
    "https://www.subscribestar.com/posts/43", "https://www.dlsite.com/maniax/work/=/product_id/RJ123456.html",
    # Cloud links are handled before parsing; the parser itself refuses them
    "https://gofile.io/d/hf2SP4", "https://mega.nz/file/abc#key", "https://drive.google.com/file/d/x/view",
]
for s in SERVICES:
    LINKS += [f"https://pawchive.pw/{s}/user/u1", f"https://cum.st/{s}/user/u1/post/p1"]

out = []
for link in LINKS:
    r = LinkParser.parse(link)
    d = r.to_dict()
    d["extra_data"] = r.extra_data
    out.append({"input": link, "result": d})

path = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "links.json")
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w", encoding="utf-8") as f:
    json.dump(out, f, indent=1, ensure_ascii=False)
print(f"{len(out)} links -> {os.path.relpath(path, ROOT)}")
