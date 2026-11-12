"""Writes the reference answers of the Decompressor window's Python bridge (bridge/decompressor_bridge.py,
removed once ported) for the Rust port's test (src-tauri/src/bridge/decompressor.rs). It ran against
the commit before its removal.

    python tools/golden_decompress_bridge.py

Cases: the window's list (a header row per creator and folder, then its archives) with searches and
folded groups; the selection buttons and Clear finished, with the counts after each; the folders
scanned (the watchlist's folders, the default folder with a creator's subfolder, custom folders) over
a made-up folder tree. Paths under the temporary folder are written as <B>.
Output: desktop/src-tauri/tests/fixtures/decompress_bridge.json
"""

import json
import os
import shutil
import sys
import tempfile
from types import SimpleNamespace

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from PySide6.QtCore import QCoreApplication  # noqa: E402

from bridge.decompressor_bridge import DecompressorBridge  # noqa: E402
from services.bulk_decompressor import ArchiveItem  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "decompress_bridge.json")

ITEMS = [
    # id, creator, directory, scan_root, filename, size, selected, status
    ("a1", "Alice", r"C:\dl\Alice [patreon]\post 1", r"C:\dl\Alice [patreon]", "pack.zip", 1000, True, "pending"),
    ("a2", "Alice", r"C:\dl\Alice [patreon]\post 2", r"C:\dl\Alice [patreon]", "Art Book.7z", 2500, False, "done"),
    ("a3", "Alice", r"D:\other\alice", r"D:\other\alice", "more.rar", 300, True, "error"),
    ("b1", "Bob", r"C:\dl\Bob", r"c:\DL\bob\\", "bob_stuff.zip", 40, True, "pending"),
    ("b2", "Bob", r"C:\dl\Bob\sub", r"C:/dl/Bob", "Second.ZIP", 60, True, "password_required"),
    ("u1", "", r"E:\loose", "", "nameless.zip", 7, False, "pending"),
    ("c1", "Céline", r"C:\dl\Céline", r"C:\dl\Céline", "日本語.zip", 123456789012, True, "done"),
]


def make_items():
    out = []
    for i, c, d, root, fn, size, sel, st in ITEMS:
        it = ArchiveItem(item_id=i, path=os.path.join(d, fn), filename=fn, directory=d, size=size, creator=c,
                         scan_root=root, selected=sel, status=st)
        if st == "done":
            it.progress = 100.0
            it.extracted_present = True
            it.extracted_dir = os.path.join(d, os.path.splitext(fn)[0])
        if st == "error":
            it.error_message = "Data error"
        out.append(it)
    return out


def snapshot(b):
    rows = b._build_rows()
    return {"rows": rows, "groupCount": b.groupCount, "anyCollapsed": b.anyCollapsed, "totalCount": b.totalCount,
            "selectedCount": b.selectedCount, "selectedBytes": b.selectedBytes, "totalBytes": b.totalBytes}


def main():
    app = QCoreApplication.instance() or QCoreApplication([])  # noqa: F841
    out = {"items": [it.to_dict() for it in make_items()]}

    # ── The list, searched and folded ──
    b = DecompressorBridge(None, None)
    b._items = make_items()
    views = []
    for search, collapsed in [("", []), ("alice", []), ("ZIP", []), ("other", []), ("post 2", []), ("nobody", []),
                              ("", ["Alice|C:\\dl\\Alice [patreon]"]), ("", ["Bob|c:\\DL\\bob\\\\", "Unknown|E:\\loose"])]:
        b._search = search.strip().lower()
        b._collapsed = set(collapsed)
        views.append({"search": search, "collapsed": collapsed, **snapshot(b)})
    b._search, b._collapsed = "", set()
    b.setAllCollapsed(True)
    views.append({"search": "", "collapsed": "all", **snapshot(b)})
    b.setAllCollapsed(False)
    b.toggleGroupCollapsed("Bob|c:\\DL\\bob\\\\")
    views.append({"search": "", "collapsed": "toggled", **snapshot(b)})
    out["views"] = views

    # ── Selection buttons ──
    b = DecompressorBridge(None, None)
    b._items = make_items()
    steps = []
    for op, args in [("toggleItem", ["a2"]), ("toggleItem", ["zz"]), ("toggleCreatorByScanRoot", ["Alice", "C:\\dl\\Alice [patreon]"]),
                     ("toggleCreatorByScanRoot", ["Alice", "C:\\dl\\Alice [patreon]"]), ("toggleCreatorByScanRoot", ["Bob", "C:\\dl\\Bob"]),
                     ("selectCreatorByScanRoot", ["Unknown", "E:\\loose", True]), ("selectCreatorByScanRoot", ["", "E:\\loose", True]),
                     ("selectCreator", ["Alice", False]), ("selectAll", [False]), ("selectAll", [True]),
                     ("toggleCreator", ["Bob"]), ("clearFinished", [])]:
        getattr(b, op)(*args)
        steps.append({"op": op, "args": args, **snapshot(b)})
    out["selection"] = steps

    # ── Folders scanned ──
    base = tempfile.mkdtemp(prefix="pawchive_decbridge_")
    try:
        B = os.path.realpath(base)
        for d in ["dl/Alice [patreon]", "dl/bob", "dl/Carol [fanbox]/x", "own/Dave", "own/dave [fantia]", "custom one", "Eve"]:
            os.makedirs(os.path.join(B, d.replace("/", os.sep)), exist_ok=True)
        open(os.path.join(B, "dl", "Frank [patreon]"), "w").close()       # a file, not a folder
        entries = [
            {"creator_name": "Alice", "service": "patreon", "user_id": "1", "download_dir": ""},
            {"creator_name": "Bob", "service": "", "user_id": "2", "download_dir": ""},
            {"creator_name": "Carol", "service": "fanbox", "user_id": "3", "download_dir": os.path.join(B, "dl")},
            {"creator_name": "Dave", "service": "fantia", "user_id": "4", "download_dir": os.path.join(B, "own") + " "},
            {"creator_name": "", "service": "x", "user_id": "55", "download_dir": os.path.join(B, "own", "Dave")},
            {"creator_name": "Ghost", "service": "patreon", "user_id": "6", "download_dir": os.path.join(B, "missing")},
            {"creator_name": "Frank", "service": "patreon", "user_id": "7", "download_dir": ""},
            {"creator_name": "Alice", "service": "patreon", "user_id": "8", "download_dir": os.path.join(B, "dl", "Alice [patreon]")},
        ]
        wm = SimpleNamespace(entries=[SimpleNamespace(**e) for e in entries])
        cases = []
        for name, default, custom in [
            ("default folder", os.path.join(B, "dl"), [os.path.join(B, "custom one"), os.path.join(B, "EVE"), os.path.join(B, "dl", "bob")]),
            ("no default folder", "", [os.path.join(B, "custom one")]),
            ("missing default folder", os.path.join(B, "nope"), []),
        ]:
            app_bridge = SimpleNamespace(downloadDir=default)
            b = DecompressorBridge(wm, app_bridge)
            b._custom_locations = [{"path": p, "creator": os.path.basename(p), "source": "custom"} for p in custom]
            got = b._get_all_target_locations()
            cases.append({"name": name, "download_dir": default, "custom": custom, "locations": got})
        out["locations"] = {"entries": entries, "cases": cases}
        text = json.dumps(out, ensure_ascii=False, indent=1)
        text = text.replace(json.dumps(B)[1:-1], "<B>")
    finally:
        shutil.rmtree(base, ignore_errors=True)
    with open(FIXTURE, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print(f"wrote {FIXTURE}: {len(out['views'])} views, {len(out['selection'])} selection steps, {len(cases)} location cases")


if __name__ == "__main__":
    main()
