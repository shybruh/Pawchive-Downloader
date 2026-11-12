"""Writes the reference answers of the Python link/password extractor and file ordering for the
Rust port's test.

    python tools/golden_extract.py

Output: desktop/src-tauri/tests/fixtures/extract.json. The Rust versions
(src-tauri/src/downloader/extract.rs and order.rs) must give the same answers.
"""

import base64
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core.file_order import name_sort_key, order_files  # noqa: E402
from services.link_extractor import LinkExtractor as LE, is_non_media_host  # noqa: E402

OUT = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "extract.json")

b64 = lambda s: base64.b64encode(s.encode()).decode()

URLS = [
    "", "  ", "https://mega.nz/file/abc#key", "https://mega.nz/#!abc!key!", "(https://mega.nz/folder/x#y).",
    "//drive.google.com/file/d/abc/view", "mega.nz/file/zzz", "dropbox.com/s/abc?dl=0,", "&quot;https://gofile.io/d/AbC&quot;",
    "https://example.com/a?x=1&amp;y=2", "https://pawchive.pw/patreon/user/1", "https://pawchive.pw/external?url=https%3A%2F%2Fmega.nz%2Ffile%2Fq",
    "https://kemono.su/x?url=https://pixeldrain.com/u/abc", "https://cdn.example.com/app.js", "https://x.com/style.CSS",
    "http://localhost:8000/a", "https://nodot/a", "ftp://x.com/a", "https://[::1/a", "https://user@pawchive.pw/a",
    "https://pawchive.pw:443/a", "https://pawchive.pw/external?link=&url=", "https://a.b/c?x=1&copy=2", "https://a.b/c?x=1&amp",
    "https://a.b/&#x41;&#66;", "https://a.b/&notit;", "https://pawchive.pw/external?url=https%3A%2F%2Fexample.com%2Fa.js",
    "'https://bunkr.si/a/xyz'", "{https://catbox.moe/abc.zip}", "https://mega.io/folder/x#F!key!",
]

TEXTS = [
    "",
    'Get it <a href="https://mega.nz/file/abc#key">here</a> or <a href=https://www.mediafire.com/file/x/y.zip>mirror</a>.',
    "[Download](https://drive.google.com/file/d/123/view) and [other](https://example.org/page)",
    "raw https://gofile.io/d/AbCd, and //pixeldrain.com/u/xyz plus mega.nz/folder/qq#zz and dropbox.com/s/abc",
    "defanged hxxps://mega[.]nz/file/abc and h**ps://pixeldrain(dot)com/u/zz",
    "encoded " + b64("https://mega.nz/file/encoded#key") + " and " + b64("mega.nz/file/nolink") + " junk QUJDREVGR0hJSktMTU5PUA==",
    '<iframe src="https://www.youtube.com/embed/abc123"></iframe> https://bunkr.si/a/qwe https://balbums.st/a/z',
    "https://1fichier.com/?abc https://terabox.com/s/1x https://workupload.com/file/x https://qiwi.gg/file/y "
    "https://catbox.moe/a.zip https://erome.com/a/abc https://nhentai.net/g/1/ https://saint2.su/d/x https://simpcity.su/t/x",
    "pawchive inside https://pawchive.pw/patreon/user/1 and https://pawchive.pw/external?url=https%3A%2F%2Fwww.dropbox.com%2Fs%2Fq",
    "x " * 50 + "https://mega.nz/file/far#k " + "y " * 400 + "password: farpw",
    "Password: hunter22\nhttps://mega.nz/file/near#k",
    "<a href='https://www.dropbox.com/scl/fi/x?rlkey=1&amp;dl=0'>d</a>",
    "https://docs.google.com/document/d/1/edit https://drive.usercontent.google.com/download?id=2",
    "日本語 https://mega.nz/file/jp#k パスワード：nihon123です",
]

PASSWORDS = [
    "", "password: hunter2", "PW = abc123, more", "pass:x", "Password is s3cret.", "[PW: zipme]", "(password=bracket)",
    "パスワードは「kagi123」です", "パス：lamb2024になります", "解凍パス abcdefです。", "【パス: 鍵abc】", "解压密码为：20240901请勿外传",
    "密碼: tw_pass", "пароль: секрет123", "비밀번호: 한국pw12", "mot de passe: motpw!", "Passwort: geheim",
    "pw: http://not", "password: null", "password: a", "pw: x" * 1, "pd: 1234 pd: 1234", "zip pass: zz-top", "pw:「quoted」",
    "Password: abc日本語", "pwd: ok:fine", "password — dashpw", "pw: “curly”", "pw: tail]】", "encryption key: K3Y!!",
    "pw: &amp;amp", "pass: abc　def", "pw: " + "x" * 70, "pw: ab；cd", "パスワード abc123を入力",
]

POSTS = [
    {"title": "Links https://mega.nz/file/t#k", "content": '<a href="https://gofile.io/d/abc">x</a>', "captionHtml": "",
     "embed": {"url": "https://www.youtube.com/watch?v=xyz", "description": "see https://dropbox.com/s/q"},
     "comments_text": "mirror https://pixeldrain.com/u/c", "comments": [{"content": "https://catbox.moe/z.zip"}, "https://erome.com/a/s"],
     "attachments": [{"path": "https://mediafire.com/file/a/b", "name": "x"}, {"path": "/ab/cd/x.zip"}],
     "file": {"path": "//drive.google.com/file/d/m"}},
    {"embed": "https://vimeo.com/123456", "content": '<iframe src="https://player.vimeo.com/video/987?h=abc"></iframe> '
     "https://streamable.com/e/qq https://www.redgifs.com/watch/foo-bar https://x.com/u/status/1 https://paypal.me/me "
     "https://soundcloud.com/a/b https://bilibili.com/video/BV1 https://dailymotion.com/video/x7 https://youtu.be/s_h-1"},
    {"embed": {"src": "https://ko-fi.com/me"}, "caption": "https://picarto.tv/videopopout/abc"},
    {"title": 12345, "content": None},
    {},
]

VAULT_POSTS = [
    {"id": 7, "title": "  Pack  ", "published": "2026-01-02T00:00:00", "content": "<p>Get it: https://mega.nz/file/AAA#key<br>Password: hunter2</p>"
     "<p>Mirror https://gofile.io/d/xyz pw: other1</p>", "embed": {"url": "https://www.youtube.com/watch?v=abc"}},
    {"post_id": "9", "added": "2025-05-05", "description": "no links here, pass: lonely", "post_url": "https://x/p/9"},
    {"edited": "2024", "caption": "https://pixeldrain.com/u/qq and https://pixeldrain.com/u/qq again", "url": "https://x/u"},
    "not a post",
]

# Posts pointing to other posts: the one-hop look at them (the target post and its creator's comments)
SITE = {
    "posts": {"555": {"id": "555", "user": "42", "title": "Master post", "content": "All files: https://mega.nz/folder/F1#k2 password: master9",
                      "description": ""},
              "666": {"id": "666", "user": "42", "title": "Rules", "content": "nothing", "description": ""}},
    "comments": {"555": [{"commenter": "42", "content": "backup https://dropbox.com/s/zz"}, {"commenter": "99", "content": "https://spam.example/x"}],
                 "666": [{"user": "42", "content": "pw for all: shared7 https://catbox.moe/q.zip"}]},
}


class _Site:
    def fetch_single_post(self, parsed):
        return SITE["posts"].get(str(parsed.post_id))

    def fetch_post_comments(self, domain, service, user_id, post_id):
        return SITE["comments"].get(str(post_id), [])


CROSS_POSTS = [
    {"id": "100", "title": "See", "content": "Links in https://pawchive.pw/patreon/user/42/post/555 and https://pawchive.pw/patreon/user/42/post/555 "
     "and https://pawchive.pw/patreon/user/42/post/100 own https://mega.nz/file/X#y", "user": "42"},
    {"id": "101", "content": "https://www.patreon.com/posts/rules-666 https://pawchive.pw/patreon/user/42/post/404", "user_id": "42"},
]

NAMES = [
    "Mya2v10.png", "Mya2.png", "Mya2v2.png", "page 9.jpg", "page_10.jpg", "page-11.jpg", "一枚目.png", "二枚目.png", "十枚目.png",
    "Ａ２.png", "a2.png", "étude.png", "etude.png", "й.png", "и.png", "", "  ", "cover_.jpg", "cover.jpg", "Vol. 2", ".hidden",
    "file.", "file 2.png", "file.png", "a 1.png", "a1.png", "a b.png", "二〇二四.png", "一万五千.png", "二十.png", "十一.png",
    "٣.png", "३.png", "１０.png", "9.png", "ｶﾀｶﾅ.png", "かたかな.png", "カタカナ.png", "เก.png", "กเ.png", "x/y\\z.png",
    "ß.png", "ss.png", "a.verylongextension", "①.png", "²x.png", "Ⅻ.png", "𝟗.png", "file~1.png", "file·2.png",
]


def main():
    cases = []
    for u in URLS:
        cases.append({"fn": "clean_and_normalize_url", "args": {"s": u}, "result": LE.clean_and_normalize_url(u)})
        cases.append({"fn": "categorize_url", "args": {"s": u}, "result": LE.categorize_url(u)})
        cases.append({"fn": "is_non_media_host", "args": {"s": u}, "result": is_non_media_host(u)})
    for h in ["https://paypal.me/x", "https://www.ko-fi.com/a", "https://notpaypal.me/a", "https://user@discord.gg:80/x", "https://[discord.gg]/"]:
        cases.append({"fn": "is_non_media_host", "args": {"s": h}, "result": is_non_media_host(h)})
    for t in TEXTS + PASSWORDS:
        cases.append({"fn": "links_from_text", "args": {"s": t}, "result": LE.extract_links_from_text(t)})
        cases.append({"fn": "rescue", "args": {"s": t}, "result": LE.rescue_obfuscated_and_base64(t)})
        cases.append({"fn": "passwords_with_positions", "args": {"s": t},
                      "result": [[p["password"], p["start"], p["end"]] for p in LE.extract_passwords_with_positions(t)]})
        cases.append({"fn": "links_with_details", "args": {"s": t}, "result": LE.extract_links_with_details(t)})
    for p in PASSWORDS:
        cases.append({"fn": "clean_password_candidate", "args": {"s": p.split(":", 1)[-1]}, "result": LE._clean_password_candidate(p.split(":", 1)[-1])})
    for post in POSTS:
        cases.append({"fn": "links_from_post", "args": {"post": post}, "result": LE.extract_links_from_post(post)})
        cases.append({"fn": "embed_urls", "args": {"post": post}, "result": LE.extract_embed_urls(post)})
    # Link Vault records (with a made-up site for the posts they point to)
    for post in POSTS + VAULT_POSTS:
        for where in (("pawchive.pw", "patreon", "42"), ("", "", "")):
            cases.append({"fn": "post_vault_record", "args": {"post": post, "where": list(where)},
                          "result": LE.extract_post_vault_record(post, None, *where)})
    for post in CROSS_POSTS:
        cases.append({"fn": "post_vault_record", "args": {"post": post, "where": ["pawchive.pw", "patreon", "42"], "site": SITE},
                      "result": LE.extract_post_vault_record(post, _Site(), "pawchive.pw", "patreon", "42")})
    for order in ["posted", "reversed", "name", "NAME ", "bogus"]:
        cases.append({"fn": "order_files", "args": {"names": NAMES, "order": order}, "result": order_files(NAMES, order, lambda n: n)})
    # Pairwise comparisons pin down the sort key itself
    for a in NAMES:
        for b in NAMES:
            ka, kb = name_sort_key(a), name_sort_key(b)
            if a < b:
                cases.append({"fn": "cmp", "args": {"a": a, "b": b}, "result": (ka > kb) - (ka < kb)})
    with open(OUT, "w", encoding="utf-8") as fh:
        json.dump(cases, fh, ensure_ascii=False, indent=1)
    print(f"{len(cases)} cases -> tests/fixtures/extract.json")


if __name__ == "__main__":
    main()
