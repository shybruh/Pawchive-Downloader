"""Writes the reference answers of the Python Google Drive download (services/cloud_downloader.py
download_gdrive_link, which runs gdown) for the Rust port's test (src-tauri/src/downloader/gdrive.rs).

    python tools/golden_gdrive.py

The download runs against made-up Drive pages: each case lists what the sites answer
([url, status, headers, body (base64)]; a 3xx with a Location is followed, anything else is 404)
and records what was logged (the download folder written as <T>), returned and saved (unfinished
".part" files with their random part as ????????).
Output: desktop/src-tauri/tests/fixtures/gdrive.json
"""

import base64
import json
import os
import re
import shutil
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

import requests  # noqa: E402
from requests.structures import CaseInsensitiveDict  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "gdrive.json")
SITE = []


class _Resp:
    def __init__(self, url, status, headers, body):
        self.url = url
        self.status_code = status
        self.headers = CaseInsensitiveDict(headers or {})
        self.content = body
        self.text = body.decode("utf-8", "replace")

    def raise_for_status(self):
        if self.status_code >= 400:
            raise requests.HTTPError(f"HTTP {self.status_code}")

    def iter_content(self, chunk_size=1):
        for i in range(0, len(self.content), chunk_size):
            yield self.content[i:i + chunk_size]

    def json(self):
        return json.loads(self.text)

    def close(self):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *a):
        pass


def _get(url, follow=True, req_headers=None):
    for _ in range(10):
        for u, status, headers, body, *ranges in SITE:
            if u == url:
                data = base64.b64decode(body)
                rng = (req_headers or {}).get("Range")
                if ranges and rng:        # (this one answers byte ranges)
                    start = int(rng[6:-1])
                    headers = dict(headers, **{"Content-Range": f"bytes {start}-{len(data) - 1}/{len(data)}",
                                               "Content-Length": str(len(data) - start)})
                    r = _Resp(url, 206, headers, data[start:])
                else:
                    r = _Resp(url, status, headers, data)
                break
        else:
            return _Resp(url, 404, {}, b"")
        loc = r.headers.get("Location")
        if follow and 300 <= r.status_code < 400 and loc:
            url = loc
            continue
        return r
    return r


class _Session:
    def __init__(self, *a, **k):
        self.headers = {}
        self.cookies = requests.cookies.RequestsCookieJar()

    def get(self, url, headers=None, **k):
        return _get(url, True, headers)

    def mount(self, *a):
        pass

    def close(self):
        pass


requests.session = _Session
requests.Session = _Session
requests.get = lambda url, allow_redirects=True, **k: _get(url, allow_redirects)

import gdown  # noqa: E402,F401
gd = sys.modules["gdown.download"]     # (the package's "download" is the function)
gd._load_cookies = lambda **k: requests.cookies.RequestsCookieJar()
gd._save_cookies = lambda **k: None
from core.filter_engine import FilterOptions  # noqa: E402
from services import cloud_downloader as cd  # noqa: E402


def b64(b):
    return base64.b64encode(b if isinstance(b, bytes) else b.encode()).decode()


def content(n, seed):
    return bytes((seed + i * 37) % 251 for i in range(n))


def cd_file(name, data, extra=None):
    h = {"Content-Type": "application/octet-stream", "Content-Disposition": f'attachment; filename="{name}"',
         "Content-Length": str(len(data)), "Last-Modified": "Wed, 21 Oct 2026 07:28:00 GMT"}
    h.update(extra or {})
    return h


UC = "https://drive.google.com/uc?id="
UCD = "https://drive.usercontent.google.com/download?id="

I1, I2, I3, I4, I5 = ("1AbCdEfGhIjKlMnOpQrStUvWxYz01", "1AbCdEfGhIjKlMnOpQrStUvWxYz02", "1AbCdEfGhIjKlMnOpQrStUvWxYz03",
                      "1AbCdEfGhIjKlMnOpQrStUvWxYz04", "1AbCdEfGhIjKlMnOpQrStUvWxYz05")
DOC, SHEET = "1DocDocDocDocDocDocDocDocDoc01", "1ShtShtShtShtShtShtShtShtSht01"
F1, F2, F3 = "1FolderFolderFolderFolderFo01", "1FolderFolderFolderFolderFo02", "1FolderFolderFolderFolderFo03"
FA, FB, FC, FD = "1FileFileFileFileFileFileFi0A", "1FileFileFileFileFileFileFi0B", "1FileFileFileFileFileFileFi0C", "1FileFileFileFileFileFileFi0D"


def redirect(frm, to):
    return [frm, 303, {"Location": to}, b64(b"")]


def page(html):
    return {"Content-Type": "text/html; charset=utf-8"}


def html(url, body, status=200):
    return [url, status, {"Content-Type": "text/html; charset=utf-8"}, b64(body)]


QUOTA = ('<html><head><title>Google Drive - Quota exceeded</title></head><body>\n<p class="uc-error-subcaption">'
         "Too many users have viewed or downloaded this file recently. Please try accessing the file again later. "
         "If the file you are trying to access is particularly large or is shared with many people, it may take up "
         "to 24 hours to be able to view or download the file.</p></body></html>")


def folder_view(title, entries):
    rows = "".join(f'<div class="flip-entry" id="entry-x"><a href="{href}" target="_blank"><div class="flip-entry-info">'
                   f'<div class="flip-entry-title">{name}</div></div></a></div>\n' for href, name in entries)
    return f"<!DOCTYPE html><html><head><title>{title}</title></head><body><div class=\"flip-entries\">\n{rows}</div></body></html>"


CASES = []
# 1. A small file: its name, then the file
d1 = content(5000, 1)
CASES.append([f"https://drive.google.com/file/d/{I1}/view?usp=sharing", [
    redirect(UC + I1, UCD + I1 + "&export=download"), [UCD + I1 + "&export=download", 200, cd_file("clip one.mp4", d1), b64(d1)]]])
# 2. A big file: Drive's "download anyway" form first
d2 = content(7000, 2)
form = (f'<!DOCTYPE html><html><body><form id="download-form" action="https://drive.usercontent.google.com/download" method="get">'
        f'<input type="submit" value="Download anyway"><input type="hidden" name="id" value="{I2}">'
        f'<input type="hidden" name="export" value="download"><input type="hidden" name="confirm" value="t">'
        f'<input type="hidden" name="uuid" value="ab-12 cd"></form></body></html>')
CASES.append([f"https://drive.google.com/file/d/{I2}/view", [
    redirect(UC + I2, UCD + I2 + "&export=download"), html(UCD + I2 + "&export=download", form),
    [f"https://drive.usercontent.google.com/download?id={I2}&export=download&confirm=t&uuid=ab-12+cd", 200,
     cd_file("Big file ü.zip", d2, {"Content-Disposition": "attachment; filename*=UTF-8''Big%20file%20%C3%BC.zip"}), b64(d2)]]])
# 3. The older confirmation link
d3 = content(300, 3)
CASES.append([f"https://drive.google.com/open?id={I3}", [
    [f"https://drive.google.com/open?id={I3}", 302, {"Location": f"https://drive.google.com/file/d/{I3}/view"}, b64("")],
    html(UC + I3, f'<a id="uc-download-link" href="/uc?export=download&amp;confirm=Xy1_&amp;id={I3}">Download</a>'),
    [f"https://docs.google.com/uc?export=download&confirm=Xy1_&id={I3}", 200, cd_file("old.pdf", d3), b64(d3)]]])
# 4. Over quota
CASES.append([f"https://drive.google.com/file/d/{I4}/view", [html(UC + I4, QUOTA)]])
# 5. Private
CASES.append([f"https://drive.google.com/file/d/{I5}/view", [html(UC + I5, "<html><body>Sign in</body></html>")]])
# 6. A Google document (exported)
dd = content(900, 6)
CASES.append([f"https://docs.google.com/document/d/{DOC}/edit", [
    redirect(UC + DOC, f"https://docs.google.com/document/d/{DOC}/edit"), html(f"https://docs.google.com/document/d/{DOC}/edit", "<html>doc</html>"),
    [f"https://docs.google.com/document/d/{DOC}/export?format=docx", 200, cd_file("My: Doc.docx", dd), b64(dd)]]])
# 7. Left out by the filters
CASES.append([f"https://drive.google.com/file/d/{I1}/view", [
    redirect(UC + I1, UCD + I1 + "&export=download"), [UCD + I1 + "&export=download", 200, cd_file("wip clip.mp4", d1), b64(d1)]]])
# 8. Another Drive error
CASES.append([f"https://drive.google.com/file/d/{I2}/view", [
    html(UC + I2, '<p class="uc-error-subcaption">Sorry, the file you have requested does not exist.</p>')]])
# 9. Not a Drive link Drive knows
CASES.append(["https://drive.google.com/drive/my-drive", []])
# 10. The connection ends early (the ".part" stays)
CASES.append([f"https://drive.google.com/file/d/{I3}/view", [
    [UC + I3, 200, cd_file("cut.bin", content(4000, 5)), b64(content(1000, 5))]]])
# 11. A folder: files, a sheet, a subfolder, a quota page saved as a file, one the filters leave out
da, db, dc = content(2000, 11), content(1500, 12), content(800, 13)
ds = content(600, 14)
CASES.append([f"https://drive.google.com/drive/folders/{F1}?usp=sharing", [
    html(f"https://drive.google.com/embeddedfolderview?id={F1}", folder_view("Pack: one", [
        (f"https://drive.google.com/file/d/{FA}/view?usp=drive_web", "a.jpg"),
        (f"https://docs.google.com/spreadsheets/d/{SHEET}/edit?usp=drive_web", "Sheet"),
        (f"https://drive.google.com/drive/folders/{F2}", "Sub / dir"),
        (f"https://drive.google.com/file/d/{FD}/view?usp=drive_web", "q.txt"),
        ("https://example.com/other", "other")])),
    html(f"https://drive.google.com/embeddedfolderview?id={F2}", folder_view("Sub", [
        (f"https://drive.google.com/file/d/{FB}/view", "b.png"), (f"https://drive.google.com/file/d/{FC}/view", "wip c.jpg")])),
    [UC + FA, 200, cd_file("a.jpg", da), b64(da)],
    redirect(UC + SHEET, f"https://docs.google.com/spreadsheets/d/{SHEET}/edit"),
    html(f"https://docs.google.com/spreadsheets/d/{SHEET}/edit", "<html>sheet</html>"),
    [f"https://docs.google.com/spreadsheets/d/{SHEET}/export?format=xlsx", 200, cd_file("Sheet.xlsx", ds), b64(ds)],
    [UC + FB, 200, cd_file("b.png", db), b64(db)],
    [UC + FC, 200, cd_file("wip c.jpg", dc), b64(dc)],
    [UC + FD, 200, cd_file("q.txt", QUOTA.encode()), b64(QUOTA)]]])
# 12. A folder Drive won't show
CASES.append([f"https://drive.google.com/drive/folders/{F3}", []])
# 13. A folder with a file that doesn't finish
CASES.append([f"https://drive.google.com/drive/u/0/folders/{F2}", [
    html(f"https://drive.google.com/embeddedfolderview?id={F2}", folder_view("Sub", [
        (f"https://drive.google.com/file/d/{FB}/view", "b.png"), (f"https://drive.google.com/file/d/{FC}/view", "c.jpg")])),
    [UC + FB, 200, cd_file("b.png", db), b64(db)],
    [UC + FC, 200, cd_file("c.jpg", content(5000, 1)), b64(content(10, 1))]]])
# 14. An empty folder
CASES.append([f"https://drive.google.com/drive/folders/{F3}", [
    html(f"https://drive.google.com/embeddedfolderview?id={F3}", folder_view("Empty", []))]])

# 15. Continued from the ".part" an earlier try left
CASES.append([f"https://drive.google.com/file/d/{I3}/view", [
    [UC + I3, 200, cd_file("cut.bin", content(4000, 5), {"ETag": '"e1"'}), b64(content(4000, 5)), True]],
    {"cut.binabcdefgh.part": b64(content(1000, 5))}])

OPTIONS = {"skip_words": "wip"}


def tree(base):
    out = {}
    for root, _, files in os.walk(base):
        for f in files:
            p = os.path.join(root, f)
            rel = os.path.relpath(p, base).replace("\\", "/")
            rel = re.sub(r"[a-z0-9_]{8}\.part$", "????????.part", rel)
            with open(p, "rb") as fh:
                out[rel] = b64(fh.read())
    return out


def main():
    global SITE
    out = []
    for url, site, *setup in CASES:
        SITE = site
        base = tempfile.mkdtemp()
        target = os.path.join(base, "dl")
        for name, data in (setup[0] if setup else {}).items():
            os.makedirs(target, exist_ok=True)
            with open(os.path.join(target, name), "wb") as fh:
                fh.write(base64.b64decode(data))
        logs = []
        opts = FilterOptions.from_dict(OPTIONS)
        ok = cd.download_gdrive_link(url, target, log_func=logs.append, skip_words=opts.skip_words, options=opts)
        logs = [re.sub(r"[a-z0-9_]{8}\.part", "????????.part", l.replace(target, "<T>")) for l in logs]
        out.append({"url": url, "site": site, "setup": setup[0] if setup else {}, "ok": bool(ok), "logs": logs, "files": tree(target) if os.path.isdir(target) else {}})
        shutil.rmtree(base, ignore_errors=True)
    names = ["a/b\\c.txt", " x.mp4 ", "CON.txt", "nul .txt", "com¹.x", "conin$", "..", "", "a<b>:c?.", "tab\x01x", "ok"]
    sane = [[n, gd._sanitize_filename(filename=n)] for n in names]
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump({"options": OPTIONS, "cases": out, "sanitized": sane}, fh, ensure_ascii=False, indent=1)
    print(f"{len(out)} downloads -> {FIXTURE}")
    for c in out:
        print(c["url"][:60], c["ok"], sorted(c["files"]), "|", " / ".join(l.strip() for l in c["logs"])[:400])


if __name__ == "__main__":
    main()
