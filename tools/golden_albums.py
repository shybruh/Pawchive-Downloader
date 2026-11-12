"""Writes the reference answers of the Python album clients (services/bunkr_client.py,
erome_client.py, nhentai_client.py) for the Rust port's test (src-tauri/src/downloader/albums.rs).

    python tools/golden_albums.py

The clients run against made-up pages instead of the sites: each case lists the pages it may ask
for ([method, url, post id, status, body]; anything else answers 404) and what the client returned.
Output: desktop/src-tauri/tests/fixtures/albums.json
"""

import json
import os
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

import requests  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "albums.json")

PAGES = []          # the pages of the case running now


class _Resp:
    def __init__(self, status, body):
        self.status_code = status
        self.content = body.encode("utf-8")
        self.text = body

    def json(self):
        return json.loads(self.text)

    def raise_for_status(self):
        if self.status_code >= 400:
            raise requests.HTTPError(f"HTTP {self.status_code}")


class _Session:
    def __init__(self, *a, **k):
        pass

    def _answer(self, method, url, pid=""):
        for i, (m, u, p, status, body) in enumerate(PAGES):
            if m == method and u == url and p == pid:
                if isinstance(status, list):            # (several answers in turn: the last one stays)
                    s = status.pop(0) if len(status) > 1 else status[0]
                    return _Resp(s, body)
                return _Resp(status, body)
        return _Resp(404, "")

    def get(self, url, headers=None, timeout=None):
        return self._answer("GET", url)

    def post(self, url, headers=None, json=None, timeout=None):
        return self._answer("POST", url, str((json or {}).get("id", "")))

    def close(self):
        pass


requests.Session = _Session
time.sleep = lambda s: None

from services import bunkr_client, erome_client, nhentai_client  # noqa: E402

SIGN = "https://glb-apisign.cdn.cr/sign"
META = "https://dl.bunkr.cr/api/_001_v2"
OK_SIGN = '{"token": "TOK", "ex": 1790000000}'


def meta(path, original, media="https://mlk.cdn.cr"):
    return json.dumps({"mediafiles": media, "path": path, "original": original})


def album(title, body):
    return f"<html><head><title>{title} | Bunkr</title></head><body>{body}</body></html>"


CASES = [
    # ── Bunkr ──
    ["bunkr", "https://c.scdn.st/storage/media/abc.mp4?n=My%20Video%2B1%3A.mp4&token=x&ex=1", []],
    ["bunkr", "https://c.cdn.cr/storage/media/xyz.mkv", []],
    ["bunkr", "https://dl.bunkr.cr/file/123", [
        ["POST", META, "123", 200, meta("/storage/media/x y.mp4", "x y.mp4")],
        ["GET", SIGN + "?path=/storage/media/x%20y.mp4", "", 200, OK_SIGN]]],
    ["bunkr", "https://dl.bunkr.cr/file/124", [      # no details: then it's not a file page either
        ["POST", META, "124", 200, "{}"]]],
    ["bunkr", "https://bunkr.cr/f/AbC-1", [
        ["GET", "https://bunkr.cr/f/AbC-1", "", 200,
         '<title>clip &amp; more.mp4 | Bunkr</title><script>var jsCDN = "https:\\/\\/x.cdn.cr\\/storage\\/media\\/q.mp4"; '
         'var signUrl = "https://sign2.cdn.cr/s";</script>'],
        ["GET", "https://sign2.cdn.cr/s?path=/storage/media/q.mp4", "", 200, OK_SIGN]]],
    ["bunkr", "https://bunkr.si/v/zz9", [
        ["GET", "https://bunkr.si/v/zz9", "", 200,
         '<meta property="og:title" content="Vid: one?.mp4"><a id="download-btn" class="b" data-id="777">dl</a>'],
        ["POST", META, "777", 200, meta("/storage/media/v1.mp4", "")],
        ["GET", SIGN + "?path=/storage/media/v1.mp4", "", 200, OK_SIGN]]],
    ["bunkr", "https://bunkr.cr/i/img1", [
        ["GET", "https://bunkr.cr/i/img1", "", 200,
         "<script>var ogname = 'photo.JPG';</script><img src=\"https://i.bunkr.ru/photo.JPG\">"]]],
    ["bunkr", "https://bunkr.cr/d/doc", [
        ["GET", "https://bunkr.cr/d/doc", "", 200, "<title>nothing here</title>"]]],
    ["bunkr", "https://bunkr.cr/f/signfail", [
        ["GET", "https://bunkr.cr/f/signfail", "", 200, '<title>a.mp4</title><script>var jsCDN = "https://x.cdn.cr/storage/media/a.mp4";</script>'],
        ["GET", SIGN + "?path=/storage/media/a.mp4", "", 429, ""]]],
    ["bunkr", "https://bunkr.cr/a/ALB1", [
        ["GET", "https://bunkr.cr/a/ALB1?advanced=1", "", 200, album(
            "Summer &#39;24 set",
            "<script>window.albumFiles = [{id: 11, name: 'one.mp4', slug: 's1', size: 1000}, "
            "{id: 12, original: \"two.jpg\", slug: \"s2\", size: 20}, {slug: 's3'}, {id: 14, name: 'gone.mp4', size: 5}];</script>")],
        ["POST", META, "11", 200, meta("/storage/media/one.mp4", "one.mp4")],
        ["GET", SIGN + "?path=/storage/media/one.mp4", "", 200, OK_SIGN],
        ["POST", META, "12", 500, ""],
        ["GET", "https://bunkr.cr/f/s2", "", 200, '<title>two.jpg | Bunkr</title><a href="https://dl.bunkr.cr/file/912">x</a>'],
        ["POST", META, "912", 200, meta("/storage/media/two.jpg", "two.jpg")],
        ["GET", SIGN + "?path=/storage/media/two.jpg", "", 200, OK_SIGN],
        ["GET", "https://bunkr.cr/f/s3", "", 200, '<title>three.webm</title><video src="https://media.bunkr.ru/three.webm"></video>']]],
    ["bunkr", "https://balbums.st/a/BAL", [
        ["GET", "https://bunkr.cr/a/BAL?advanced=1", "", 200, album(
            "Cards",
            '<div class="grid-images"><div class="theItem relative"><a href="/f/c1" title="Card one.mp4"><p class="theName x">Card One.mp4</p></div>\n</div>'
            '<div class="theItem"><a href="/f/c2" title="t2.jpg"></a></div></div>')],
        ["GET", "https://bunkr.cr/f/c1", "", 200, "<title>c1.mp4</title><source src='https://m.bunkr.ru/c1.mp4'>"],
        ["GET", "https://bunkr.cr/f/c2", "", 200, "<title>c2.jpg</title>"]]],
    ["bunkr", "https://bunkr.cr/a/LNK?x=1", [
        ["GET", "https://bunkr.cr/a/LNK?x=1&advanced=1", "", 200, album(
            "Links", '<a href="https://bunkr.cr/f/l1">1</a><a href="/v/l2/">2</a><a href="/f/l1">dup</a>')],
        ["GET", "https://bunkr.cr/f/l1", "", 200, '<title>l1.zip</title><a href="https://f.bunkr.ru/l1.zip">z</a>'],
        ["GET", "https://bunkr.cr/f/l2", "", 200, '<title>l2.zip</title><a href="https://f.bunkr.ru/l2.zip">z</a>']]],
    ["bunkr", "https://bunkr.cr/a/LEG", [
        ["GET", "https://bunkr.cr/a/LEG?advanced=1", "", 200, album(
            "Legacy", '<a href="https://cdn9.bunkr.ru/b%20c.mp4?x=1">v</a><a href="/i/pic.png">p</a>'
                      '<a href="https://cdn9.bunkr.ru/a.zip">z</a><a href="https://other.example/page">no</a>')]]],
    ["bunkr", "https://bunkr.cr/a/RETRY", [
        ["GET", "https://bunkr.cr/a/RETRY?advanced=1", "", [429, 503], ""],
        ["GET", "https://bunkr.cr/a/RETRY", "", 200, album("Fallback &amp; co", "")]]],
    ["bunkr", "https://bunkr.cr/a/MISSING", []],
    ["bunkr", "https://bunkr.cr/a/EMPTY", [
        ["GET", "https://bunkr.cr/a/EMPTY?advanced=1", "", 200, "<html><body>no title</body></html>"]]],
    # ── Erome ──
    ["erome", "https://www.erome.com/a/AbC123", [
        ["GET", "https://www.erome.com/a/AbC123", "", [429, 200],
         '<meta property="og:title" content="Beach &amp; sun / day 1">'
         '<video><source src="https://v12.erome.com/1/AbC123/v1_720p.mp4" type="video/mp4"></video>'
         '<img data-src="https://s12.erome.com/1/AbC123/p1.jpg?v=2" alt="" class="img-front lasyload">'
         '<img src="https://s12.erome.com/1/AbC123/thumbs/thumbnail.jpg" class="img-front">'
         'https://s7.erome.com/1/AbC123/P2.JPEG and https://v12.erome.com/1/AbC123/v1_720p.mp4 again'
         '<source src="https://v1.erome.com/noext/xyz">']]],
    ["erome", "https://www.erome.com/a/NoTitle", [
        ["GET", "https://www.erome.com/a/NoTitle", "", 200, '<source src="https://v1.erome.com/a.webm">']]],
    ["erome", "https://www.erome.com/a/Gone", [["GET", "https://www.erome.com/a/Gone", "", 404, ""]]],
    ["erome", "https://www.erome.com/user/x", []],
    # ── nHentai ──
    ["nhentai", "https://nhentai.net/g/123456/", [
        ["GET", "https://nhentai.net/api/v2/galleries/123456", "", 200, json.dumps({
            "title": {"pretty": "A: tale?", "english": "Long"}, "media_id": "999",
            "pages": [{"t": "j"}, {"t": "p"}, {"t": "w"}, {"t": "g"}, {"t": "x"}, {}]})]]],
    ["nhentai", "654321", [
        ["GET", "https://nhentai.net/api/v2/galleries/654321", "", [429, 200], json.dumps({
            "title": {"pretty": "", "english": "English <one>"}, "media_id": 5, "pages": [{"t": "j"}]})]]],
    ["nhentai", "777", [
        ["GET", "https://nhentai.net/api/v2/galleries/777", "", 200, json.dumps({"title": {}, "media_id": None, "pages": []})]]],
    ["nhentai", "888", [["GET", "https://nhentai.net/api/v2/galleries/888", "", 404, ""]]],
    ["nhentai", "no id here", []],
]


def main():
    global PAGES
    out = []
    for kind, url, pages in CASES:
        PAGES = json.loads(json.dumps(pages))       # (a copy: the turns are used up)
        fn = {"bunkr": bunkr_client.fetch_bunkr_album, "erome": erome_client.fetch_erome_album,
              "nhentai": nhentai_client.fetch_nhentai_gallery}[kind]
        title, files = fn(url)
        out.append({"kind": kind, "url": url, "pages": pages, "title": title, "files": files})
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"{len(out)} cases -> {FIXTURE}")
    for c in out:
        print(c["kind"], c["url"][:50], "->", c["title"], len(c["files"]))


if __name__ == "__main__":
    main()
