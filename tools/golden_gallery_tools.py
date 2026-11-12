"""Writes the reference answers of the Gallery's Python tools (bridge/gallery_tools_bridge.py and
core/post_metadata_index.py, removed once ported) for the Rust port's test
(src-tauri/src/bridge/gallery_tools.rs). It ran against the commit before their removal.

    python tools/golden_gallery_tools.py

Cases: splitting a search into words, file types and tags; reading post_info.txt into posts;
searches over a made-up folder tree with post texts (names, post titles / text / tags); new folders;
moves and renames undone. Paths under the temporary folder are written as <B>.
Output: desktop/src-tauri/tests/fixtures/gallery_tools.json
"""

import json
import os
import shutil
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from bridge import gallery_tools_bridge as gt  # noqa: E402
from core import post_metadata_index as pmi  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "gallery_tools.json")

QUERIES = ["", "  cat  dog ", "ext:png", "ext:.JPG", "*.mp4 cat", "*.", ".gif", ".", "tag:Blue", 'tag:"Long Tag" x',
           '"two words"', "'single quoted'", 'unbalanced "quote', "a\\ b", 'mix"ed"word', "TAG:Upper EXT:Png Word",
           "tag:", "ext:", "日本語 タグ", 'tag:"a b" ext:zip "c d" e']

POST_TEXTS = [
    "",
    "Title: Hello\nPost ID: 12345\nCreator: Artist [patreon]\nTags: Blue, Red ,, green\n--- Content ---\nSome body text\nmore\n--- Attached Files ---\nA.PNG\nb.jpg\n",
    "Title: One\nPost ID: 1\n" + "=" * 40 + "\nTitle: Two\nPost ID: 2\nCreator: NoService\n--- Attached Files ---\nx.png\n",
    "Title: Win\r\nPost ID: 9\r\nCreator: Who [fanbox] x\r\n",
    "just text without header colon\n",
    "=" * 30 + "\n\n" + "=" * 50,
    "Key: value: with colon\nTitle:   spaced   \n--- Other ---\nignored: yes\n--- Content ---\n" + "z" * 5000 + " tailword\n",
]

# The made-up tree: "path": contents (None: a folder)
TREE = {
    "Artist/cat_one.png": "x" * 10,
    "Artist/dog.jpg": "x" * 20,
    "Artist/post_info.txt": "Title: Sunset Painting\nPost ID: 111\nCreator: Artist [patreon]\nTags: Landscape, Orange Sky\n--- Content ---\nA calm evening scene\n--- Attached Files ---\ncat_one.png\n",
    "Artist/Post A/img1.png": "x" * 30,
    "Artist/Post A/img2.gif": "x" * 40,
    "Artist/Post A/post_info.txt": "Title: Blue Hour\nPost ID: 222\nTags: Blue, Night\n--- Attached Files ---\nimg1.png\nimg2.gif\n",
    "Artist/Shared/p1.png": "x",
    "Artist/Shared/p2.png": "xx",
    "Artist/Shared/p3.mp4": "xxx",
    "Artist/Shared/post_info [1].txt": "Title: Forest walk\nPost ID: 1\nTags: Green\n--- Attached Files ---\np1.png\n",
    "Artist/Shared/post_info [2].txt": "Title: City lights\nPost ID: 2\nTags: Night, Neon\n--- Attached Files ---\np2.png\np3.mp4\n",
    "Artist/Grouped/Images/g1.png": "x" * 5,
    "Artist/Grouped/Other/post_info.txt": "Title: Grouped post\nTags: Typed\n",
    "Artist/Empty/": None,
    "Artist/Cat Folder/": None,
    "Artist/Cat Folder/inner.txt": "hi",
    "Other/CAT.PNG": "xxxxxx",
    "Other/notes.TXT": "y",
}

SEARCHES = ["cat", "png", "ext:png", "*.gif", "sunset", "blue", "tag:blue", "tag:night", "night ext:mp4", "tag:neon *.png",
            "calm evening", "forest", "typed", "folder", "zzz", "tag:orange", "artist", "patreon", "cat ext:png", "post"]


def rel(v, base):
    if isinstance(v, str):
        return v.replace(base, "<B>")
    if isinstance(v, list):
        return [rel(x, base) for x in v]
    if isinstance(v, dict):
        return {k: rel(x, base) for k, x in v.items()}
    return v


def build(base):
    for p, c in TREE.items():
        full = os.path.join(base, p.replace("/", os.sep))
        if c is None:
            os.makedirs(full, exist_ok=True)
            continue
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w", encoding="utf-8", newline="") as f:
            f.write(c)


def listing(base):
    out = []
    for root, dirs, files in os.walk(base):
        for n in dirs + files:
            out.append(os.path.relpath(os.path.join(root, n), base).replace("\\", "/"))
    return sorted(out)


class FakeApp:
    """The app bridge's moveItems / renameItem as the Rust one answers them (already ported)"""

    def getPathSafetyInfo(self, p):
        return {"is_blocked": False}

    def moveItems(self, paths, dest):
        pairs, errors = [], []
        for p in paths:
            src = os.path.normpath(p)
            target = os.path.join(os.path.normpath(dest), os.path.basename(src))
            if os.path.exists(target):
                errors.append(f"An item named '{os.path.basename(src)}' already exists in the destination")
                continue
            shutil.move(src, target)
            pairs.append([src, target])
        return {"moved": len(pairs), "failed": len(errors), "errors": errors, "pairs": pairs}

    def renameItem(self, path, name):
        dst = os.path.join(os.path.dirname(os.path.normpath(path)), name)
        os.rename(path, dst)
        return {"success": True, "error": "", "new_path": dst}


def main():
    out = {"queries": [[q, list(gt.parse_search_query_extended(q))] for q in QUERIES]}
    out["posts"] = [[t, [{"post_id": r.post_id, "title": r.title, "creator": r.creator, "service": r.service, "tags": r.tags,
                          "content": r.content, "attached": sorted(r.attached_files), "blob": r.search_blob}
                         for r in pmi.parse_post_info_content(t)]] for t in POST_TEXTS]

    base = os.path.realpath(tempfile.mkdtemp())
    build(base)
    b = gt.GalleryToolsBridge(app_bridge=FakeApp(), marks_file=os.path.join(base, "_marks.json"))
    searches = []
    for q in SEARCHES:
        got = []
        b.searchBatch.connect(lambda tok, items, done, scanned, got=got: got.append([items, done, scanned]))
        words, exts, tags = gt.parse_search_query_extended(q)
        b._run_search(b._search_token, base, words, exts, tags, 5000)
        b.searchBatch.disconnect()
        items = [dict(i, mtime=None) for batch in got for i in batch[0]]
        searches.append({"query": q, "items": rel(items, base), "scanned": got[-1][2]})
    got = []
    b.searchBatch.connect(lambda tok, items, done, scanned: got.append([items, done, scanned]))
    b._run_search(b._search_token, base, ["png"], [], [], 3)
    b.searchBatch.disconnect()
    searches.append({"query": "png", "max": 3, "items": rel([dict(i, mtime=None) for x in got for i in x[0]], base), "scanned": got[-1][2]})
    out["search"] = {"tree": TREE, "runs": searches}

    # ── New folders, moves and renames, undone ──
    steps = []

    def step(op, *args):
        if op == "createFolder":
            r = b.createFolder(os.path.join(base, args[0].replace("/", os.sep)) if args[0] else "", args[1])
        elif op == "moveItems":
            r = b.moveItems([os.path.join(base, p.replace("/", os.sep)) for p in args[0]], os.path.join(base, args[1].replace("/", os.sep)))
        elif op == "renameItem":
            r = b.renameItem(os.path.join(base, args[0].replace("/", os.sep)), args[1])
        elif op == "undo":
            r = b.undoLast()
        steps.append({"op": op, "args": list(args), "result": rel(r, base), "canUndo": b.canUndo, "undoLabel": b.undoLabel,
                      "files": listing(base)})

    if os.path.exists(os.path.join(base, "_marks.json")):
        os.remove(os.path.join(base, "_marks.json"))
    step("createFolder", "Other", "New")
    step("createFolder", "Other", "New")
    step("createFolder", "Other", "bad:name")
    step("createFolder", "Other", "trail.")
    step("createFolder", "Other", " ")
    step("createFolder", "", "x")
    step("createFolder", "Missing", "x")
    step("moveItems", ["Other/CAT.PNG", "Other/notes.TXT"], "Other/New")
    step("renameItem", "Other/New/CAT.PNG", "renamed.png")
    step("createFolder", "Other/New", "Deep")
    step("undo")
    step("undo")
    step("undo")
    step("createFolder", "Other", "Holder")
    with open(os.path.join(base, "Other", "Holder", "keep.txt"), "w") as f:
        f.write("k")
    step("undo")
    step("undo")
    step("undo")
    step("moveItems", ["Artist/dog.jpg"], "Other")
    with open(os.path.join(base, "Artist", "dog.jpg"), "w") as f:
        f.write("again")
    step("undo")
    step("moveItems", ["Artist/cat_one.png"], "Other")
    os.remove(os.path.join(base, "Other", "cat_one.png"))
    step("undo")
    step("undo")
    for i in range(27):
        b.createFolder(os.path.join(base, "Other"), f"f{i}")
    steps.append({"op": "count", "args": [], "result": len(b._undo), "canUndo": b.canUndo, "undoLabel": b.undoLabel, "files": []})
    out["ops"] = {"steps": steps}
    shutil.rmtree(base, ignore_errors=True)

    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"{len(QUERIES)} queries, {len(POST_TEXTS)} post texts, {len(searches)} searches, {len(steps)} steps -> {FIXTURE}")
    for s in searches:
        print(s["query"], s["scanned"], [i["path"] for i in s["items"]])
    for s in steps:
        print(s["op"], s["args"], json.dumps(s["result"])[:150], s["undoLabel"])


if __name__ == "__main__":
    main()
