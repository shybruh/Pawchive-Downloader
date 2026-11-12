"""Writes the reference answers of the Python download planner (FileDownloader.build_tasks_from_posts)
for the Rust port's test.

    python tools/golden_plan.py           # made-up posts covering the planner's rules
    python tools/golden_plan.py --live    # also the example creator's real posts (not committed)

Output:
  desktop/src-tauri/tests/fixtures/plan.json — the made-up cases (committed)
  desktop/src-tauri/target/plan_live.json    — the real posts (for: cargo test --lib -- --ignored plan)

Each case runs in desktop/src-tauri/target/plan_base with the files the case lists created first.
The download archive, archive passwords and post_info writes are
stand-ins, so nothing outside that folder is read or written. The Rust planner
(src-tauri/src/downloader/plan.rs) must give the same tasks.
"""

import copy
import json
import os
import shutil
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

import core.downloader as dl  # noqa: E402
import core.archive_password_manager as apm  # noqa: E402
from core import folder_naming  # noqa: E402
from core.filter_engine import FilterOptions  # noqa: E402

BASE = os.path.join(ROOT, "desktop", "src-tauri", "target", "plan_base")
FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "plan.json")
LIVE = os.path.join(ROOT, "desktop", "src-tauri", "target", "plan_live.json")

# Stand-ins, mirrored by the Rust test
class Archive:
    def __init__(self, archived):
        self.archived = set(archived)
        self.is_enabled = bool(archived)

    def is_archived(self, service, post_id, file_id, file_hash=""):
        return file_id in self.archived

    def record_link(self, **kw):
        pass


apm.archive_password_manager.record_archive_password = lambda *a, **k: None
apm.archive_password_manager.add_passwords = lambda *a, **k: None
WRITES = []
dl._write_post_info = lambda path, content, post_id="": WRITES.append([path, post_id, content])


def h(n):
    """A fake content hash path /ab/cd/<64 hex>.ext"""
    x = f"{n:064x}"
    return f"/{x[:2]}/{x[2:4]}/{x}"


def P(pid, title, files=(), atts=(), **kw):
    post = {"id": pid, "user": "152819670", "service": "patreon", "title": title, "published": "2024-03-05T10:00:00"}
    if files:
        post["file"] = files[0]
    post["attachments"] = list(atts)
    post.update(kw)
    return post


PAW = dict(creator="Silly Teshii", service="patreon", domain="pawchive.pw", user_id="152819670")
CUM = dict(creator="Cum Creator", service="onlyfans", domain="cum.st", user_id="ccc")

BASIC = [
    P("1", "First post", [{"name": "cover.jpg", "path": h(1) + ".jpg"}],
      [{"name": "pic.png", "path": h(2) + ".png", "bytes": 1000}, {"name": "pic.png", "path": h(3) + ".png"},
       {"name": "clip.mp4", "path": h(4) + ".mp4", "node": 2}, {"name": "pic.png", "path": h(2) + ".png"},
       {"name": "doc.zip,", "path": h(5) + ".zip"}, {"name": "locked.png", "path": h(6) + ".png", "locked": True}],
      content='Text <br> with password: hunter22 and <a href="https://mega.nz/file/x#y">mega</a> '
              '<img src="https://img.pawchive.pw/thumbnail/data' + h(7) + '.jpg"> <iframe src="https://www.youtube.com/embed/abc"></iframe>'),
    P("2", "First post", [], [{"name": "pic.png", "path": h(8) + ".png"}, {"name": "", "path": h(9) + ".webp", "mimeType": "image/webp"},
                               {"path": h(10), "mimeType": "video/mp4; codecs=x"}, {"name": "noext", "path": h(11), "mimeType": "audio/mpeg"}]),
    P("3", "Text only", content="Just words. Mirror https://gofile.io/d/AbC pass: zz-top", comments_text="pw: comm1"),
    P("4", "", [], [{"name": "a.png", "path": "https://n4.pawchive.pw/data" + h(12) + ".png?f=a.png"},
                    {"name": "big.mp4", "path": "https://t1.pawchive.pw/f/884966d8/big.mp4?e=99&s=ab", "is_temporary": True},
                    {"name": "prev.jpg", "path": h(13) + ".jpg", "preview_only": True},
                    {"name": "thumb.png", "path": "https://img.pawchive.pw/thumbnail/data" + h(14) + ".png"},
                    {"name": "x_low.jpg", "path": h(15) + ".jpg"}, {"name": "kem.png", "path": "https://kemono.su/data" + h(16) + ".png"}],
      published=1709632800),
    P("5", "Miku dance", [{"name": "Miku.mp4", "path": h(17) + ".mp4"}], [{"name": "Miku 2.mp4", "path": h(18) + ".mp4"}],
      tags=["vocaloid", "dance"], embed={"url": "https://vimeo.com/123456"}),
    P("https://example.com/album/1", "Album", [], [{"name": "pic.png", "path": h(19) + ".png"}], published=None, user=None),
    P("7", "Samus", [{"name": "preview.jpg", "path": h(20) + ".jpg"}], [], tags="metroid,samus"),
]

CUMST = [
    {"id": "c1", "user": "ccc", "service": "onlyfans", "title": "Cum post", "published": "2023-01-02",
     "file": {"name": "", "path": "ab" * 32, "mimeType": "image/jpeg", "variants": [{"name": "720p.jpg"}, {"name": "original.jpg"}]},
     "attachments": [{"name": "vid.mp4", "path": "cd" * 32, "variants": [{"name": "240p.mp4"}, {"name": "480p.mp4"}]},
                     {"name": "e1.jpg", "path": "https://e1.cum.st/media/" + "ef" * 32 + "/original.jpg"},
                     {"name": "plain.gif", "path": "https://cum.st/data" + h(21) + ".gif"}]},
]

O = lambda **kw: kw
CASES = [
    ("defaults", BASIC, PAW, O(), [], []),
    ("no subfolders", BASIC, PAW, O(subfolder_per_post=False), [], []),
    ("no date prefix, index prefix", BASIC, PAW, O(date_prefix=False, file_index_prefix=True), [], []),
    ("group by post", BASIC, PAW, O(group_file_type="post"), [], []),
    ("group by creator", BASIC, PAW, O(group_file_type="creator"), [], []),
    ("group by creator, flat", BASIC, PAW, O(group_file_type="creator", subfolder_per_post=False), [], []),
    ("skip covers", BASIC, PAW, O(skip_post_covers=True), [], []),
    ("thumbnails only", BASIC, PAW, O(download_thumbnails_only=True), [], []),
    ("thumbnail fallback", BASIC, PAW, O(fallback_to_thumbnails=True), [], []),
    ("keep duplicates", BASIC, PAW, O(keep_duplicates=True, subfolder_per_post=False), [], []),
    ("no content images, no embeds", BASIC, PAW, O(scan_content_images=False, download_embeds=False), [], []),
    ("images only", BASIC, PAW, O(file_type="images"), [], []),
    ("exact extensions", BASIC, PAW, O(exact_extensions="png, mp4"), [], []),
    ("name order", BASIC, PAW, O(file_order="name"), [], []),
    ("reversed", BASIC, PAW, O(file_order="reversed"), [], []),
    ("numbered names", BASIC, PAW, O(filename_style="numbered"), [], []),
    ("links only", BASIC, PAW, O(file_type="links"), [], []),
    ("no post info", BASIC, PAW, O(save_post_metadata=False), [], []),
    ("filename template", BASIC, PAW, O(filename_style="custom", filename_template="{date} {title} {index} {orig_name}"), [], []),
    ("skip words", BASIC, PAW, O(skip_words="dance", skip_scope="both"), [], []),
    ("on disk", BASIC, PAW, O(), ["Silly Teshii [patreon]/[2024-03-05] First post/pic.png", "Silly Teshii [patreon]/[2024-03-05] First post/clip.mkv",
                                    "Silly Teshii [patreon]/[2024-03-05] First post/post_info.txt:Post ID: 1"], []),
    ("on disk, small copy", BASIC, PAW, O(), ["Silly Teshii [patreon]/[2024-03-05] First post/pic.png"], []),
    ("shared post info", BASIC, PAW, O(subfolder_per_post=False), ["Silly Teshii [patreon]/post_info.txt:Post ID: 3"], []),
    ("archive", BASIC, PAW, O(), [], ["1_" + h(2) + ".png", "embed_5_1"]),
    ("no site in folder name", BASIC, dict(PAW, include_service=False), O(), [], []),
    ("existing plain folder", BASIC, PAW, O(), ["Silly Teshii/keep.txt"], []),
    ("artist folder inside base", BASIC, dict(PAW, artist_dir="{BASE}\\Chosen"), O(), [], []),
    ("artist folder elsewhere", BASIC, dict(PAW, artist_dir="{BASE}\\..\\elsewhere\\Artist"), O(), [], []),
    ("batch id", BASIC, dict(PAW, batch_id="b1"), O(), [], []),
    ("cum.st", CUMST, CUM, O(), [], []),
    ("cum.st thumbnails", CUMST, CUM, O(fallback_to_thumbnails=True), [], []),
    ("date range", BASIC, PAW, O(date_after="2024-03-06"), [], []),
]


def setup(files):
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    for spec in files:
        rel, _, content = spec.partition(":")
        p = os.path.join(BASE, rel.replace("/", os.sep))
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as fh:
            fh.write(content or ("x" * 100000))


def run(posts, req, opts, archived):
    folder_naming.include_service = req.get("include_service", True)
    d = dl.FileDownloader(None, archive_manager=Archive(archived))
    d._resolve_pawchive_deferred_attachments = lambda post, domain="": 0
    WRITES.clear()
    artist = req.get("artist_dir")
    tasks = d.build_tasks_from_posts(copy.deepcopy(posts), req["creator"], req["service"], req["domain"], BASE,
                                     FilterOptions.from_dict(opts), batch_id=req.get("batch_id"),
                                     artist_dir=artist.replace("{BASE}", BASE) if artist else None, user_id=req["user_id"])
    sub = lambda s: s.replace(BASE, "{BASE}") if isinstance(s, str) else s
    out = []
    for t in tasks:
        td = t.to_dict()
        td.pop("options", None)
        td["post_info_path"] = t.post_info_path
        out.append({k: sub(v) if not isinstance(v, list) else [sub(x) for x in v] for k, v in td.items()})
    return {
        "tasks": out,
        "pending_post_infos": sorted([[sub(k[0]), k[1]], c] for k, c in d._post_info_pending.items()),
        "writes": [[sub(p), i, c] for p, i, c in WRITES],
        "harvested_links": d.harvested_links,
        "harvested_records": sorted(d.harvested_links_records, key=lambda r: r["url"]),
    }


def main():
    out = []
    for name, posts, req, opts, files, archived in CASES:
        setup(files)
        out.append({"name": name, "posts": posts, "request": req, "options": opts, "files": files, "archived": archived,
                    "result": run(posts, req, opts, archived)})
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"{len(out)} cases, {sum(len(c['result']['tasks']) for c in out)} tasks -> tests/fixtures/plan.json")
    if "--live" in sys.argv:
        from core.api_client import ApiClient
        from core.parser import LinkParser
        p = LinkParser().parse("https://pawchive.pw/patreon/user/152819670")
        posts = ApiClient().fetch_user_posts(p, page_start=1, page_end=2)
        live = []
        for name, opts in [("defaults", {}), ("flat by type", {"subfolder_per_post": False, "group_file_type": "creator"}),
                           ("thumbnails", {"download_thumbnails_only": True}), ("links", {"file_type": "links"})]:
            setup([])
            live.append({"name": name, "posts": posts, "request": PAW, "options": opts, "files": [], "archived": [],
                         "result": run(posts, PAW, opts, [])})
        with open(LIVE, "w", encoding="utf-8") as fh:
            json.dump(live, fh, ensure_ascii=False)
        print(f"live: {len(posts)} posts, {sum(len(c['result']['tasks']) for c in live)} tasks -> target/plan_live.json")
    shutil.rmtree(BASE, ignore_errors=True)


if __name__ == "__main__":
    main()
