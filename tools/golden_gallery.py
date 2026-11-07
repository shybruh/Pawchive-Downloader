"""Writes the Python bridge's Gallery answers (bridge/app_bridge.py, "Integrated File Explorer")
for the Rust port's test (src-tauri/src/bridge/gallery.rs).

    .venv/Scripts/python tools/golden_gallery.py
    cd desktop/src-tauri && cargo test --lib -- --ignored gallery

A made-up folder tree (fixed sizes and times) is created in desktop/src-tauri/target/gallery_base
and listed, measured, searched for duplicates; moves and renames run on a copy of it. The safety
check is asked about system and user folders of this computer, so the answers are written to
target/gallery_live.json (not committed) and the tree is left in place for the test.
"""

import json
import os
import shutil
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from bridge.app_bridge import AppBridge  # noqa: E402

TARGET = os.path.join(ROOT, "desktop", "src-tauri", "target")
BASE = os.path.join(TARGET, "gallery_base")
OPS = os.path.join(TARGET, "gallery_ops_py")
OUT = os.path.join(TARGET, "gallery_live.json")

T0 = 1_700_000_000.25
# (relative path, content or None for a folder, modified time)
TREE = [
    ("Alpha", None, T0 + 10),
    ("Alpha/one.png", "A" * 2000, T0 + 1),
    ("Alpha/two.png", "A" * 2000, T0 + 3),               # same as one.png
    ("Alpha/three.png", "A" * 1999 + "B", T0 + 2),         # same size, different end
    ("Alpha/Deep", None, T0 + 11),
    ("Alpha/Deep/copy.png", "A" * 2000, T0 + 2.5),
    ("Alpha/Deep/big.bin", "x" * 70000 + "1", T0 + 4),
    ("Alpha/Deep/big2.bin", "x" * 70000 + "1", T0 + 5),
    ("Alpha/Deep/big3.bin", "x" * 70000 + "2", T0 + 6),    # same first 64 KB, different file
    ("beta", None, T0 + 12),
    ("beta/empty.txt", "", T0 + 7),
    ("beta/empty2.txt", "", T0 + 8),
    ("Gamma Folder", None, T0 + 13),
    ("zeta.MP4", "v" * 300, T0 + 9),
    ("Zeta2.jpg", "A" * 2000, T0 + 9.5),
    ("readme", "plain", T0 + 9.75),
    (".hidden.cfg", "h", T0 + 9.8),
    ("archive.tar.gz", "z" * 12, T0 + 9.9),
]


def build(base):
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    for rel, content, _ in TREE:
        p = os.path.join(base, rel.replace("/", os.sep))
        if content is None:
            os.makedirs(p, exist_ok=True)
        else:
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "w", encoding="utf-8", newline="") as fh:
                fh.write(content)
    for rel, _, mtime in sorted(TREE, key=lambda t: -t[0].count("/")):      # (folders after their files)
        p = os.path.join(base, rel.replace("/", os.sep))
        os.utime(p, (mtime, mtime))


class Fake:
    """The parts of the bridge the explorer uses"""
    def __init__(self):
        self._download_dir = BASE
        self._folder_stats_worker_token = 0
        self._folder_stats_cache = {}
        self.emitted = []
        fake = self

        class Sig:
            def emit(self, *a):
                fake.emitted.append(list(a))
        self.folderStatsCalculated = Sig()

    def __getattr__(self, name):
        fn = AppBridge.__dict__.get(name)
        if fn is None:
            raise AttributeError(name)
        return fn.__get__(self) if hasattr(fn, "__get__") else fn


def main():
    build(BASE)
    f = Fake()
    cases = []

    def case(fn, *args, result=None):
        cases.append({"fn": fn, "args": list(args), "result": result})

    for args in [(BASE, 1500), (os.path.join(BASE, "Alpha"), 1500), (BASE, 3), (os.path.join(BASE, "missing"), 1500),
                 (os.path.join(BASE, "readme"), 1500), (BASE + "\\Alpha\\..\\beta\\", 1500), (BASE.replace("\\", "/"), 1500)]:
        r = f.listDirectory(*args)
        case("listDirectory", *args, result={"items": r, "total": f.getLastListingTotal()})
    for p in [os.path.join(BASE, "Alpha"), os.path.join(BASE, "Alpha", "Deep"), os.path.join(BASE, "beta"), BASE]:
        f.emitted.clear()
        f._compute_folder_stats(p, f._folder_stats_worker_token)
        case("folderStats", p, result=f.emitted[-1])
    for p in [os.path.join(BASE, "Alpha", "Deep"), "C:\\", "C:\\Users", BASE + "/beta/../Alpha", "I:\\"]:
        case("getBreadcrumbs", p, result=f.getBreadcrumbs(p))
    for args in [(BASE, False), (BASE, True), (os.path.join(BASE, "Alpha"), False), (os.path.join(BASE, "nope"), True)]:
        case("scanDuplicates", *args, result=f.scanDuplicates(*args))

    home = os.path.expanduser("~")
    temp = tempfile.gettempdir()
    safety_paths = [
        "", "   ", "C:\\", "C:", "c:/windows", "C:\\Windows\\System32", "C:\\$Recycle.Bin", "C:\\$WinREAgent\\x", "C:\\Users",
        home, os.path.join(home, "Downloads"), os.path.join(home, "AppData", "Roaming", "x"), os.path.join(temp, "x"),
        os.path.join(home, ".vscode", "x"), "C:\\Program Files\\Foo", "D:\\Data\\ProgramData\\x", "D:\\", "I:\\", "I:\\Coding",
        os.path.join(ROOT, ".venv"), os.path.join(ROOT, ".venv", "Scripts"), ROOT, "C:\\Python312\\Lib", "C:\\Games\\x",
        "C:\\Downloads\\x", "C:\\Windows.old", "C:\\Applications", "C:\\tools", "C:\\nothing-here-xyz", BASE,
        "\\\\server\\share\\folder", "C:\\Users\\Public", "E:\\Movies\\venv-like", "C:\\Recovery\\x",
    ]
    for p in safety_paths:
        case("getPathSafetyInfo", p, result=f.getPathSafetyInfo(p))

    # Moves and renames on a copy (paths written with {OPS})
    build(OPS)
    sub = lambda v: json.loads(json.dumps(v).replace(json.dumps(OPS)[1:-1], "{OPS}"))  # noqa: E731
    o = lambda rel: os.path.join(OPS, rel.replace("/", os.sep))  # noqa: E731
    steps = []

    def step(fn, *args):
        r = getattr(f, fn)(*args)
        steps.append({"fn": fn, "args": sub(list(args)), "result": sub(r)})

    step("moveItems", [o("zeta.MP4"), o("readme")], o("beta"))
    step("moveItems", [o("Alpha"), o("beta/zeta.MP4"), o("missing.png"), ""], o("Alpha/Deep"))   # into itself / gone
    step("moveItems", [o("Alpha/one.png")], o("Alpha"))                                         # already there
    step("moveItems", [o("Zeta2.jpg")], o("nowhere"))
    step("moveItems", [o("Alpha/one.png")], o("beta"))
    step("moveItems", [o("Alpha/two.png")], "C:\\Windows")
    step("moveItems", [o("Gamma Folder")], o("Alpha/Deep"))
    step("renameItem", o("beta/readme"), "readme.txt")
    step("renameItem", o("beta/readme.txt"), "bad:name")
    step("renameItem", o("beta/readme.txt"), "  ")
    step("renameItem", o("beta/readme.txt"), "trailing.")
    step("renameItem", o("beta/readme.txt"), "one.png")                    # taken
    step("renameItem", o("beta/readme.txt"), "README.TXT")                 # case only
    step("renameItem", o("beta/README.TXT"), "README.TXT")                 # same name
    step("renameItem", o("gone.txt"), "x")
    step("renameItem", o("Alpha/Deep/Gamma Folder"), "Gamma")
    after = sorted(os.path.relpath(os.path.join(r, n), OPS) for r, ds, fs in os.walk(OPS) for n in ds + fs)

    with open(OUT, "w", encoding="utf-8") as fh:
        json.dump({"base": BASE, "tree": TREE, "cases": cases, "ops": {"steps": steps, "after": after}}, fh,
                  ensure_ascii=False, indent=1)
    shutil.rmtree(OPS, ignore_errors=True)
    print(f"{len(cases)} cases, {len(steps)} move / rename steps -> target/gallery_live.json")


if __name__ == "__main__":
    main()
