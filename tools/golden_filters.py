"""Writes the reference answers of the Python filters and naming for the Rust port's test.

    python tools/golden_filters.py

Output: desktop/src-tauri/tests/fixtures/filters.json — a list of {"fn", "args", "result"} cases
for core/filter_engine.py and core/folder_naming.py. The Rust versions
(src-tauri/src/downloader/filters.rs, naming.rs) must give the same result for every case.
"""

import itertools
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core.filter_engine import FilterEngine as F, FilterOptions, contains_term, fit_path_for_windows, normalize_size_str  # noqa: E402
from core import folder_naming  # noqa: E402

cases = []


def add(fn, args, result):
    cases.append({"fn": fn, "args": args, "result": result})


TEXTS = [
    "", "Hello World", "  spaced   out  ", "Tab\tand\nnewline\r\n", "a/b\\c*d?e:f\"g<h>i|j", "CON", "con", "lpt1", "NUL.txt",
    "dots...", "...leading", "__under__", "trailing. ", "emoji 🎨🔥 art", "café naïve", "e\u0301 combining", "日本語のタイトル",
    "初音ミクのイラスト", "한국어 제목", "ภาษาไทย", "Привет мир", "math ∑ ≠ ∞", "money $5 € ¥", "sym © ® ™", "a+b=c~d^e&f@g#h`i%",
    "<b>bold</b> text", "zero\u200bwidth", "ctrl\x01\x1f\x7fchars", "x" * 300, "ß straße", "İstanbul", "ﬁ ligature",
    "(Exclusive NSFW Animation Trend!💖💕) Thick Velina Soul Sucker! '' Marian & Sean''", "Part 1/2: The \"Quote\"",
    "line\u2028sep", "para\u2029sep", "nbsp\u00a0here", "full　width　space", "under___scores", "a . b", "[Sample] HD",
]

# clean_filesystem_text
for t in TEXTS:
    for max_len, fb in ((120, "Untitled"), (10, "X"), (0, "")):
        add("clean_filesystem_text", {"text": t, "max_len": max_len, "fallback": fb}, F.clean_filesystem_text(t, max_len, fb))

# contains_term
TERMS = ["", "miku", "Miku", "初音ミク", "art", "an", "c++", "a.b", "WIP", "sketch", "ミク", "straße", "e\u0301"]
HAY = ["", "Hatsune Miku art", "mikumiku", "初音ミクのイラスト", "c++ is fun", "aXb", "a.b test", "WIP sketch", "wipe",
       "Straße", "STRASSE", "café", "e\u0301 x"]
for term, hay in itertools.product(TERMS, HAY):
    add("contains_term", {"text": hay, "term": term}, contains_term(hay, term))

# sizes, durations, dates, extensions
SIZES = ["", "0", "500", "1g", "1.5 GB", "500mb", "1kb1", "2 TiB", "3 bytes", "1.", ".5m", "abc", "1.2.3", "10 X", "  7  ", "1e3", "-5"]
for s in SIZES:
    add("normalize_size_str", {"s": s}, normalize_size_str(s))
    add("parse_size_str", {"s": s}, F._parse_size_str(s))
SKIPS = ["", "WIP, sketch", "[>=500KB]", "[1GB-2GB]", "[2GB..1GB]", "[1.5GB to 3GB]", "[<=2GB]", "[<500MB]", "[1000]", "[size: 10MB+]",
         "[>1g]", "[10 MB]", "[foo]", "preview, [>=1MB]", "[1GB–2GB]", "[ 5 MB <= 10 MB ]"]
for s in SKIPS:
    add("size_range", {"skip_words": s}, list(F.get_file_size_range_bytes(s)))
for b in [0, 1, 1023, 1024, 1536, 10 ** 6, 1048576, 5 * 1024 ** 3 + 123, 3 * 1024 ** 4, 999999999999]:
    add("format_size_str", {"n": b}, F.format_size_str(b))
DATES = ["", "2024", "2024-06", "2024/6/5", "2024.02", "2023-02", "2024-02", "2024-13", "abcd", "2024-06-15", "24-1-2", "2024-06-15T10:00"]
for d in DATES:
    add("normalize_date", {"s": d}, F.normalize_date(d))
    add("expand_date_start", {"s": d}, F.expand_date_start(d))
    add("expand_date_end", {"s": d}, F.expand_date_end(d))
for e in ["", "zip, .rar, *.7z", "MP4;webm  gif", ".", "*", ",,,", "Tar.Gz"]:
    add("parse_extensions_list", {"raw": e}, sorted(F.parse_extensions_list(e)))
for n in ["", "a.JPG", "b.mp4", "c.zip", "d.psd", "e.mp3", "f.txt", "g", ".hidden", "h.tar.gz", "i.MKV", "j.ts"]:
    add("file_type_category", {"filename": n}, F.get_file_type_category(n))

# FilterOptions normalisation (from_dict → to_dict)
OPTION_INPUTS = [
    {}, {"min_file_size": "1g", "max_file_size": "500", "group_file_type": "POST", "file_order": "Name", "webp_quality": "nope",
         "sidecar_format": "TXT", "sidecar_scope": "x", "page_start": "3"},
    {"skip_words": "WIP", "characters": "Miku/初音ミク", "filename_template": "", "exact_extensions": "  mp4  "},
]
for o in OPTION_INPUTS:
    add("options", {"d": o}, FilterOptions.from_dict(o).to_dict())

# Filters
POSTS = [
    {"title": "Miku WIP sketch", "content": "<p>Hello <a href='x'>WIP</a></p>", "published": "2024-06-15T10:00:00"},
    {"title": "Final art", "content": "初音ミクのイラスト", "published": "2023-01-02"},
    {"title": "Untitled", "content": "", "published": ""},
    {"title": "Rin & Len", "content": "<b>sketch</b> inside", "added": "2025-12-31", "comments_text": "Miku cameo"},
    {"title": "Old", "published": "0000-00-00"},
]
OPTS = [
    {},
    {"skip_words": "WIP, sketch", "skip_scope": "posts"},
    {"skip_words": "wip", "skip_scope": "files"},
    {"characters": "miku", "character_scope": "title"},
    {"characters": "(Rin/Len), Miku", "character_scope": "comments"},
    {"characters": "初音ミク", "character_scope": "content"},
    {"characters": "miku", "character_scope": "all"},
    {"date_after": "2024", "date_before": "2024-12"},
    {"date_after": "2025-01-01", "date_before": "2023"},
    {"min_file_size": "1MB", "max_file_size": "10MB"},
    {"skip_words": "preview, [>=500KB]", "skip_scope": "both"},
    {"file_type": "images"}, {"file_type": "videos"}, {"file_type": "archives"}, {"file_type": "audio"},
    {"exact_extensions": "mp4, gif"}, {"skip_archives": True}, {"skip_archives": True, "exact_extensions": "zip"},
    {"download_thumbnails_only": True},
]
FILES = [("a.jpg", None), ("WIP_v2.png", 2_000_000), ("final-wip.mp4", 50_000_000), ("song.mp3", 400),
         ("pack.zip", 900_000_000), ("clip.webm", 5_000_000), ("long.mp4", 5_000_000), ("初音ミク.gif", 2048),
         ("", 10), ("notes.txt", 100), ("preview_image.jpg", 600_000)]
for post, opt in itertools.product(POSTS, OPTS):
    keep, reason = F.should_keep_post(post, FilterOptions.from_dict(opt))
    add("should_keep_post", {"post": post, "options": opt}, [keep, reason])
for (name, size), opt in itertools.product(FILES, OPTS):
    keep, reason = F.should_keep_file(name, FilterOptions.from_dict(opt), file_size=size)
    add("should_keep_file", {"filename": name, "file_size": size, "options": opt}, [keep, reason])

# Names
for fname, rw in itertools.product(["cover.jpeg,", "My Art [Sample] HD.png", "x.", "name", ".hidden", "a.tar.gz",
                                   "Patreon_Post_patreon.MP4", "日本語.gif", "  spaced .jpg", "bad:name?.jpg"],
                                  ["", "patreon, [sample], HD"]):
    add("sanitize_filename", {"filename": fname, "remove_words": rw}, F.sanitize_filename(fname, FilterOptions(remove_words=rw)))

STYLES = ["post_title", "date_post_title", "date_based", "post_title_global_numbering", "custom"]
TEMPLATES = ["{title} - {orig_name}", "{date}_{artist}_{file_index}.{ext}", "{post_id}-{seq_idx}", "{year}/{month}/{day} {name}{ext}",
             "{caption}", "..{ext}", "{service}_{user_id}_{post_index}_{folder_index}"]
for style, prefix in itertools.product(STYLES, [False, True]):
    for tpl in (TEMPLATES if style == "custom" else [TEMPLATES[0]]):
        opts = FilterOptions(filename_style=style, filename_template=tpl, file_index_prefix=prefix)
        for orig, title, date, caption in [("img_001.PNG", "My Post", "2024-06-15T10:00:00", ""),
                                           ("video.mp4", "Untitled", "2023-01-02", "First line\nsecond"),
                                           ("noext", "", "", ""), ("a b.c.d.gif", "Title/With:Bad*Chars", "2024", "cap")]:
            for fi, pi, foi in [(1, 1, None), (12, 3, 7)]:
                args = {"original_filename": orig, "post_title": title, "post_date": date, "post_index": pi, "file_index": fi,
                        "folder_index": foi, "post_id": "123", "artist": "Some Artist", "service": "patreon", "user_id": "99",
                        "caption": caption, "options": opts.to_dict()}
                add("format_custom_filename", args, F.format_custom_filename(
                    orig, title, date, pi, fi, opts, folder_index=foi, post_id="123", artist="Some Artist",
                    service="patreon", user_id="99", caption=caption))

for post in [{"title": "Real title", "content": "<p>Body</p>"}, {"title": None, "caption": "Cap line 1<br>line 2"},
             {"title": "null", "captionText": "&amp; escaped &lt;3"}, {"title": "Untitled", "content": "<p>First</p><p>Second</p>"},
             {"title": "  <i>tagged</i> &amp; title  "}, {}, {"title": "Post", "text": "x" * 150}]:
    add("extract_post_title_and_caption", {"post": post}, list(F.extract_post_title_and_caption(post)))

for html in ["", "<img src='https://img.pawchive.pw/thumbnail/data/ab/cd/abcdef.jpg'>",
             '<a href="https://img.cum.st/x.png?w=1">a</a><img src="/thumbnail/data/12/34/zz.webp">',
             "<img src='https://img.kemono.su/data/ab/cd/a.gif'><img src='https://c3.coomer.st/b.jpg'>",
             "<img src='a.jpg'><img src='a.jpg?x=1'><img src='b.txt'>"]:
    add("extract_content_images", {"html": html}, F.extract_content_images(html))

for tags in [None, [], ["a", " b ", '"c"'], [{"label": "L"}, {"slug": "s"}, {"tag": "t"}, {}], '{"tag1","tag 2"}', "{a,b}",
             "x, y ,z", "{}", '{"quoted, comma",plain}', 5]:
    add("normalize_tags", {"raw": tags}, F.normalize_tags(tags))

for p in [r"C:\Downloads\Artist [patreon]\Post\file.jpg",
          "C:\\D\\" + "Very Long Folder Name " * 6 + "\\" + "Another Long Post Title " * 5 + "\\" + "file name " * 12 + ".mp4",
          "C:\\" + "a" * 240 + ".png"]:
    add("fit_path_for_windows", {"path": p}, fit_path_for_windows(p))

for inc in (True, False):
    folder_naming.include_service = inc
    for name, svc in [("Artist", "patreon"), ("Artist", ""), ("A [b]", "fanbox")]:
        add("creator_folder", {"name": name, "service": svc, "include_service": inc}, folder_naming.creator_folder(name, svc))
        add("creator_folder_names", {"name": name, "service": svc, "include_service": inc}, folder_naming.creator_folder_names(name, svc))
        for fold in ["artist", "Artist [PATREON]", "Other"]:
            add("is_creator_folder", {"folder": fold, "name": name, "service": svc, "include_service": inc},
                folder_naming.is_creator_folder(fold, name, svc))
folder_naming.include_service = True

path = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "filters.json")
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w", encoding="utf-8") as f:
    json.dump(cases, f, indent=0, ensure_ascii=False)
from collections import Counter  # noqa: E402
print(f"{len(cases)} cases -> {os.path.relpath(path, ROOT)}")
print(dict(Counter(c["fn"] for c in cases)))
