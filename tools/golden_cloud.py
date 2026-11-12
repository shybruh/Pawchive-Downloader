"""Writes the reference answers of the Python cloud downloaders (services/cloud_downloader.py: MEGA,
Dropbox, GoFile) for the Rust port's test (src-tauri/src/downloader/cloud.rs).

    python tools/golden_cloud.py

The downloaders run against made-up sites instead of the real ones (MEGA files really encrypted):
each case lists what the sites answer, and records what the downloader logged, returned and saved.
Also: cloud names made safe, and which files the filters skip.
Output: desktop/src-tauri/tests/fixtures/cloud.json
"""

import base64
import io
import json
import os
import shutil
import struct
import sys
import tempfile
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

import requests  # noqa: E402
from requests.structures import CaseInsensitiveDict  # noqa: E402
from Crypto.Cipher import AES  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "cloud.json")
SITE = []           # the answers of the case running now: [method, url, payload, status, headers, body (base64)]


class _Resp:
    def __init__(self, status, headers, body):
        self.status_code = status
        self.headers = CaseInsensitiveDict(headers or {})
        self.content = body
        self.text = body.decode("utf-8", "replace")
        self.raw = None

    def json(self):
        return json.loads(self.text)

    def raise_for_status(self):
        if self.status_code >= 400:
            raise requests.HTTPError(f"HTTP {self.status_code}")

    def iter_content(self, chunk_size=16384):
        for i in range(0, len(self.content), chunk_size):
            yield self.content[i:i + chunk_size]

    def close(self):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *a):
        pass


def _url(url, params):
    if params:
        url += "?" + "&".join(f"{k}={v}" for k, v in params.items())
    return url


def _answer(method, url, payload=None):
    for m, u, p, status, headers, body in SITE:
        if m == method and u == url and (p is None or p == payload):
            return _Resp(status, headers, base64.b64decode(body))
    return _Resp(404, {}, b"")


class _Session:
    def __init__(self, *a, **k):
        self.headers = {}
        self.cookies = requests.cookies.RequestsCookieJar()

    def get(self, url, params=None, **k):
        return _answer("GET", _url(url, params))

    def post(self, url, params=None, json=None, **k):
        return _answer("POST", _url(url, params), json)

    def close(self):
        pass


requests.Session = _Session
requests.get = lambda url, **k: _answer("GET", url)

from core import bandwidth_limiter as _bl  # noqa: E402
_bl.bandwidth_limiter.wait_for = lambda *a, **k: True
from core.auth_manager import auth_manager  # noqa: E402
from core.filter_engine import FilterOptions  # noqa: E402
from services import cloud_downloader as cd  # noqa: E402

TOKEN = {"v": ""}
auth_manager.get_credential = lambda site, kind: TOKEN["v"] if (site, kind) == ("gofile", "token") else ""


# ── Made-up MEGA files ────────────────────────────────────────────────────────

def b64u(b):
    return base64.urlsafe_b64encode(b).decode().rstrip("=")


def b64(b):
    return base64.b64encode(b).decode()


def mega_key(seed):
    return bytes((seed * 7 + i * 13) % 256 for i in range(32))


def aes_key(k32):
    p = struct.unpack(">8I", k32)
    return struct.pack(">4I", p[0] ^ p[4], p[1] ^ p[5], p[2] ^ p[6], p[3] ^ p[7])


def attrs(key16, a):
    raw = ("MEGA" + json.dumps(a)).encode()
    raw = raw.ljust((len(raw) + 15) // 16 * 16, b"\0")
    return b64u(AES.new(key16, AES.MODE_CBC, b"\0" * 16).encrypt(raw))


def encrypt(k32, data):
    return AES.new(aes_key(k32), AES.MODE_CTR, nonce=k32[16:24], initial_value=0).encrypt(data)


def content(n, seed):
    return bytes((seed + i * 31) % 251 for i in range(n))


API = "https://g.api.mega.co.nz/cs"


def mega_file_case(seed, name, size, skip=False):
    k = mega_key(seed)
    data = content(size, seed)
    url = f"https://mega.nz/file/FILE{seed}#{b64u(k)}"
    site = [["POST", API, [{"a": "g", "g": 1, "p": f"FILE{seed}"}], 200, {},
             b64(json.dumps([{"g": f"https://gfs.mega.co.nz/dl/{seed}", "s": size, "at": attrs(aes_key(k), {"n": name})}]).encode())],
            ["GET", f"https://gfs.mega.co.nz/dl/{seed}", None, 200, {}, b64(encrypt(k, data))]]
    return url, site


def mega_folder_case():
    master = bytes(range(16, 32))
    ecb = AES.new(master, AES.MODE_ECB)
    nodes, site = [], []
    folders = [("FOLD1", None, "Root: set"), ("sub01", "FOLD1", "Sub/one"), ("dot02", "sub01", "..")]
    for h, parent, name in folders:
        k16 = bytes((len(h) * 11 + i * 3) % 256 for i in range(16))
        nodes.append({"h": h, "p": parent, "t": 1, "a": attrs(k16, {"n": name}), "k": f"owner:{b64u(ecb.encrypt(k16))}"})
    files = [("f1", "FOLD1", "a.jpg", 40000, 1), ("f2", "sub01", "b?.mp4", 20000, 2), ("f3", "dot02", "c.txt", 100, 3),
             ("f4", "FOLD1", "wip-d.png", 50, 4), ("f5", "FOLD1", "gone.bin", 70, 5)]
    for h, parent, name, size, seed in files:
        k = mega_key(seed)
        nodes.append({"h": h, "p": parent, "t": 0, "s": size, "a": attrs(aes_key(k), {"n": name}), "k": f"owner:{b64u(ecb.encrypt(k))}"})
        if h == "f5":
            site.append(["POST", API + "?n=FOLD1", [{"a": "g", "g": 1, "n": h}], 200, {}, b64(b"[-9]")])
            continue
        site.append(["POST", API + "?n=FOLD1", [{"a": "g", "g": 1, "n": h}], 200, {}, b64(json.dumps([{"g": f"https://gfs.mega.co.nz/f/{h}"}]).encode())])
        site.append(["GET", f"https://gfs.mega.co.nz/f/{h}", None, 200, {}, b64(encrypt(k, content(size, seed)))])
    nodes.append({"h": "nokey", "p": "FOLD1", "t": 0, "s": 1})
    site.insert(0, ["POST", API + "?n=FOLD1", [{"a": "f", "c": 1, "r": 1}], 200, {}, b64(json.dumps([{"f": nodes}]).encode())])
    return f"https://mega.nz/folder/FOLD1#{b64u(master)}", site


def zip_bytes(files):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in files:
            z.writestr(name, data)
    return buf.getvalue()


def gofile_api(cid, page, data, status=200):
    return ["GET", f"https://api.gofile.io/contents/{cid}?page={page}&pageSize=1000&sortField=createTime&sortDirection=1",
            None, status, {}, b64(json.dumps(data).encode())]


CASES = []

# MEGA
u, s = mega_file_case(1, "Clip: one.mp4", 70000)
CASES.append(["mega", u, s, "", {}])
u, s = mega_file_case(2, "wip render.png", 500)
CASES.append(["mega", u, s, "", {}])
u, s = mega_folder_case()
CASES.append(["mega", u, s, "", {}])
CASES.append(["mega", "https://mega.nz/folder/BAD1#AAAAAAAAAAAAAAAAAAAAAA", [
    ["POST", API + "?n=BAD1", [{"a": "f", "c": 1, "r": 1}], 200, {}, b64(b"[-2]")]], "", {}])
CASES.append(["mega", "https://mega.nz/file/GONE#" + b64u(mega_key(9)), [
    ["POST", API, [{"a": "g", "g": 1, "p": "GONE"}], 200, {}, b64(b"[-9]")]], "", {}])
CASES.append(["mega", "https://mega.nz/whatever", [], "", {}])
u, s = mega_file_case(3, "short.bin", 3000)
s[1][5] = b64(base64.b64decode(s[1][5])[:1000])           # (the connection ends early)
CASES.append(["mega", u, s, "", {}])

# Dropbox
CASES.append(["dropbox", "https://www.dropbox.com/scl/fi/abc/photo.jpg?rlkey=x&dl=0", [
    ["GET", "https://www.dropbox.com/scl/fi/abc/photo.jpg?rlkey=x&dl=1", None, 200,
     {"Content-Disposition": 'attachment; filename="photo 1.jpg"', "Content-Length": "5000"}, b64(content(5000, 7))]], "", {}])
z = zip_bytes([("set/a.jpg", content(3000, 1)), ("set/wip b.png", content(10, 2)), ("c.txt", b"hello")])
CASES.append(["dropbox", "https://www.dropbox.com/sh/xyz/folder?dl=0", [
    ["GET", "https://www.dropbox.com/sh/xyz/folder?dl=1", None, 200,
     {"Content-Disposition": "attachment; filename=folder", "Content-Length": str(len(z))}, b64(z)]], "", {}])
CASES.append(["dropbox", "https://www.dropbox.com/s/q/nofile", [
    ["GET", "https://www.dropbox.com/s/q/nofile?dl=1", None, 200, {}, b64(b"plain bytes")]], "", {}])
CASES.append(["dropbox", "https://www.dropbox.com/s/q/missing.mp4?dl=0", [], "", {}])
CASES.append(["dropbox", "https://www.dropbox.com/s/q/wip.mp4", [
    ["GET", "https://www.dropbox.com/s/q/wip.mp4?dl=1", None, 200, {"Content-Disposition": 'attachment; filename="wip.mp4"'}, b64(b"x")]], "", {}])
CASES.append(["dropbox", "https://www.dropbox.com/s/q/cut.mp4", [
    ["GET", "https://www.dropbox.com/s/q/cut.mp4?dl=1", None, 200, {"Content-Length": "4000000"}, b64(content(100, 3))]], "", {}])

# GoFile
folder = {"status": "ok", "data": {"type": "folder", "name": "My / Pack", "children": {
    "c1": {"type": "file", "id": "c1", "name": "one.mp4", "size": 3000, "link": "https://store1.gofile.io/download/c1/one.mp4"},
    "c2": {"type": "folder", "id": "c2", "name": "sub"},
    "c3": {"type": "file", "id": "c3", "name": "wip two.mp4", "size": 10, "link": "https://store1.gofile.io/download/c3"},
    "c4": {"type": "file", "id": "c4", "name": "CON.txt", "size": 5, "link": "https://store1.gofile.io/download/c4"}}}}
CASES.append(["gofile", "https://gofile.io/d/AbC12", [
    gofile_api("AbC12", 1, folder),
    ["GET", "https://store1.gofile.io/download/c1/one.mp4", None, 200, {}, b64(content(3000, 4))],
    ["GET", "https://store1.gofile.io/download/c4", None, 200, {}, b64(b"hello")]], "TOKEN1", {}])
CASES.append(["gofile", "https://gofile.io/d/One1", [
    gofile_api("One1", 1, {"status": "ok", "data": {"type": "file", "id": "One1", "name": "solo.zip", "size": 4,
                                                    "link": "https://store2.gofile.io/download/One1"}}),
    ["GET", "https://store2.gofile.io/download/One1", None, 200, {}, b64(b"solo")]], "TOKEN1", {}])
CASES.append(["gofile", "https://gofile.io/d/AbC12", [], "", {}])
for st in ("error-notPremium", "error-notFound", "error-passwordRequired", "error-auth", "error-rateLimit"):
    CASES.append(["gofile", "https://gofile.io/d/E1", [gofile_api("E1", 1, {"status": st})], "TOKEN1", {}])
CASES.append(["gofile", "https://gofile.io/d/E2", [gofile_api("E2", 1, {}, 401)], "TOKEN1", {}])
CASES.append(["gofile", "https://gofile.io/d/E3", [gofile_api("E3", 1, {"status": "ok", "data": {"type": "folder", "children": {}}})], "TOKEN1", {}])
CASES.append(["gofile", "https://gofile.io/x/nothing", [], "TOKEN1", {}])
CASES.append(["gofile", "https://gofile.io/d/Cut", [
    gofile_api("Cut", 1, {"status": "ok", "data": {"type": "file", "id": "Cut", "name": "cut.bin", "size": 9000,
                                                   "link": "https://store2.gofile.io/download/Cut"}}),
    ["GET", "https://store2.gofile.io/download/Cut", None, 200, {}, b64(b"short")]], "TOKEN1", {}])

OPTIONS = {"skip_words": "wip"}


def tree(base):
    out = {}
    for root, _, files in os.walk(base):
        for f in files:
            p = os.path.join(root, f)
            with open(p, "rb") as fh:
                out[os.path.relpath(p, base).replace("\\", "/")] = b64(fh.read())
    return out


def main():
    global SITE
    out = []
    fns = {"mega": cd.download_mega_link, "dropbox": cd.download_dropbox_link, "gofile": cd.download_gofile_link}
    for kind, url, site, token, extra in CASES:
        SITE = site
        TOKEN["v"] = token
        base = tempfile.mkdtemp()
        target = os.path.join(base, "dl")
        logs = []
        opts = FilterOptions.from_dict(OPTIONS)
        kw = {"max_workers": 1} if kind != "dropbox" else {}
        ok = fns[kind](url, target, log_func=logs.append, skip_words=opts.skip_words, options=opts, **kw)
        out.append({"kind": kind, "url": url, "site": site, "token": token, "ok": bool(ok), "logs": logs,
                    "files": tree(target) if os.path.isdir(target) else {}})
        shutil.rmtree(base, ignore_errors=True)
    names = ["Part 1: Intro", "..", "...", "", None, "CON.txt", "nul", "a/b\\c", "trail. ", "x" * 250, "ok.mp4", "com1.tar.gz"]
    safe = [[n, cd._safe_name(n, "fb")] for n in names]
    skip_cases = [["a/wip/b.jpg", 10, {"skip_words": "wip"}], ["dir/ok.jpg", 10, {"skip_words": "wip"}],
                  ["clip.mp4", 10, {"file_type": "images"}], ["x.jpg", 10, {"file_type": "images"}],
                  ["big.zip", 5000000, {"max_file_size": "1MB"}], ["Wipe.jpg", 1, {"skip_words": "wip"}],
                  ["my_wip.png", 1, {"skip_words": "wip, other"}], ["[tag] x.png", 1, {"skip_words": "[tag]"}],
                  ["f.o.o/bar.jpg", 1, {"skip_words": "f.o"}]]
    skips = []
    for path, size, o in skip_cases:
        opts = FilterOptions.from_dict(o)
        skips.append([path, size, o, list(cd.should_skip_cloud_file(path, skip_words=opts.skip_words, file_size=size, options=opts))])
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump({"options": OPTIONS, "cases": out, "safe_names": safe, "skips": skips}, fh, ensure_ascii=False, indent=1)
    print(f"{len(out)} downloads, {len(safe)} names, {len(skips)} filter cases -> {FIXTURE}")
    for c in out:
        print(c["kind"], c["url"][:45], c["ok"], len(c["files"]), "|", " / ".join(l.strip() for l in c["logs"])[:300])


if __name__ == "__main__":
    main()
