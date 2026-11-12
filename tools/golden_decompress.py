"""Writes the reference answers of the Python Decompressor (services/bulk_decompressor.py and
core/archive_password_manager.py) for the Rust port's test (src-tauri/src/decompress/).

    python tools/golden_decompress.py

Cases: archive names (split parts, folder names), a scan of a made-up folder tree, the disk check,
a run of password bank operations (with a made-up Link Vault), and extractions with the bundled
7-Zip of archives it made (plain, encrypted, a wrong password, a damaged file, extracting again).
Paths under the temporary folder are written as <B>.
Output: desktop/src-tauri/tests/fixtures/decompress.json
"""

import base64
import json
import os
import shutil
import subprocess
import sys
import tempfile
import types

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core import archive_password_manager as apm_mod  # noqa: E402
from core import link_vault_manager as lvm_mod  # noqa: E402
from services import bulk_decompressor as bd  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "decompress.json")

NAMES = ["a.zip", "b.part1.rar", "b.part2.rar", "b.part01.rar", "b.part10.rar", "c.7z.001", "c.7z.002", "c.001", "c.002",
         "d.tar.gz", "e.tar.zst", "f.part001.rar", "g.zip.001", "h. .zip", "...zip", "x.TAR.BZ2", "photo.jpg", "y.part3.exe",
         "z.0010", "Vol 1.rar", "noext"]

# The made-up folder tree for the scan ("name": contents; "dir/": folder)
TREE = ["root1/a.zip", "root1/a/inner.txt", "root1/sub/b.part1.rar", "root1/sub/b.part2.rar", "root1/c.7z.001", "root1/c.7z.002",
        "root1/d.tar.gz", "root1/e.txt", "root1/f.ZIP", "root1/f/", "root1/nested/n.zip", "root1/nested/deep/m.7z",
        "root2/z.iso", "root2/dup.zip"]

VAULT = {"creators": {"patreon:1": {"creator_key": "patreon:1", "creator_name": "Artist", "name": "Artist"}},
         "posts": {"p1": {"creator_name": "Artist", "creator_id": "patreon:1", "passwords": ["vpw1", "shared"]},
                   "p2": {"creator_name": "Other", "passwords": ["opw"]}},
         "all": ["vglobal", "shared"]}

BANK_STEPS = [
    ["add", {"password": " one ", "creator": ""}], ["add", {"password": "one", "creator": "Global"}],
    ["add", {"password": "", "creator": "X"}], ["add", {"password": "art1", "creator": "Artist"}],
    ["add_many", {"passwords": ["a", "b", "a", " ", "c"], "creator": "Artist"}],
    ["add_many", {"passwords": ["g1"], "creator": ""}],
    ["remove", {"password": "b", "creator": "Artist"}], ["remove", {"password": "nope", "creator": ""}],
    ["record", {"archive": "<B>/arc/x.zip", "password": "mapped", "creator": "Artist"}],
    ["record", {"archive": "<B>/arc/y.zip", "password": "ypw", "creator": ""}],
    ["get_archive", {"archive": "<B>/ARC/X.zip"}], ["get_archive", {"archive": "<B>/arc/none.zip"}],
    ["passwords", {}], ["by_creator", {}],
    ["candidates", {"archive": "<B>/arc/x.zip", "creator": "Artist"}],
    ["candidates", {"archive": "<B>/Creator/Archive/Post/q.rar", "creator": ""}],
    ["candidates", {"archive": "<B>/shared/s.zip", "creator": "artist"}],
    ["sync", {}], ["sync", {}],
    ["remove", {"password": "a", "creator": ""}], ["remove", {"password": "mapped", "creator": ""}],
    ["passwords", {}], ["by_creator", {}],
    ["clear", {}], ["passwords", {}],
]

# Files the candidates read (post texts beside the archives)
INFO_FILES = {
    "arc/post_info.txt": "Title\n--- Detected Password(s) ---\nfrompost\nsecond\n--- Attached Files ---\nx.zip\n",
    "arc/info.txt": "password: infopw",
    "arc/notes.txt": "pass: notespw",
    "arc/extra.txt": "pw: never-read",
    "Creator/Other/Post/post_info.txt": "Password: otherdir",
    "Creator/Archive/post_info.txt": "pw: parentpw",
    "shared/post_info [11].txt": "--- Detected Password(s) ---\nwrongpost\n--- Attached Files ---\nother.zip\n",
    "shared/post_info [12].txt": "--- Detected Password(s) ---\nrightpost\n--- Attached Files ---\ns.zip\n",
}


def rel(v, base):
    if isinstance(v, str):
        return v.replace(base, "<B>")
    if isinstance(v, list):
        return [rel(x, base) for x in v]
    if isinstance(v, dict):
        return {k: rel(x, base) for k, x in v.items()}
    return v


def files_of(base):
    out = {}
    for root, dirs, files in os.walk(base):
        for f in files:
            p = os.path.join(root, f)
            with open(p, "rb") as fh:
                out[os.path.relpath(p, base).replace("\\", "/")] = base64.b64encode(fh.read()).decode()
        if not files and not dirs:
            out[os.path.relpath(root, base).replace("\\", "/") + "/"] = ""
    return out


def make_archives(seven, base):
    """Archives made by the bundled 7-Zip: the test extracts the same bytes"""
    src = os.path.join(base, "src")
    os.makedirs(os.path.join(src, "inner"))
    with open(os.path.join(src, "one.txt"), "w") as f:
        f.write("hello one")
    with open(os.path.join(src, "inner", "two.bin"), "wb") as f:
        f.write(bytes(range(256)) * 40)
    out = {}
    hide = 0x08000000 if sys.platform == "win32" else 0
    for name, extra in [("plain.zip", []), ("locked.7z", ["-ppw1", "-mhe=on"]), ("locked.zip", ["-ppw2"])]:
        p = os.path.join(base, name)
        subprocess.run([seven, "a", "-y", *extra, p, os.path.join(src, "*")], stdout=subprocess.DEVNULL, creationflags=hide, check=True)
        with open(p, "rb") as f:
            out[name] = base64.b64encode(f.read()).decode()
    out["broken.zip"] = base64.b64encode(b"PK\x03\x04 this is not really a zip" * 3).decode()
    shutil.rmtree(src)
    for name in out:
        if os.path.exists(os.path.join(base, name)):
            os.remove(os.path.join(base, name))
    return out


def main():
    out = {"names": [[n, bd.is_secondary_part(n), bd.clean_archive_stem(n)] for n in NAMES]}

    # ── Scan and disk check ──
    base = os.path.realpath(tempfile.mkdtemp())
    for t in TREE:
        p = os.path.join(base, t)
        if t.endswith("/"):
            os.makedirs(p, exist_ok=True)
            continue
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f:
            f.write(b"x" * (len(t) * 100))
    locs = [(os.path.join(base, "root1"), "A"), (os.path.join(base, "root1", "nested"), "N"), (os.path.join(base, "root2"), ""),
            (os.path.join(base, "missing"), "M"), ("", "E")]
    eng = bd.BulkDecompressorEngine()
    items = eng.scan_locations(locs)
    out["scan"] = {"locations": rel([list(x) for x in locs], base), "tree": TREE, "items": rel([i.to_dict() for i in items], base)}
    for i in items:
        i.selected = i.filename != "dup.zip"
    out["disk"] = [{k: v for k, v in r.to_dict().items() if k not in ("freeBytes", "isSufficient")}
                   for r in eng.check_disk_space(items)]
    shutil.rmtree(base, ignore_errors=True)

    # ── Password bank ──
    base = os.path.realpath(tempfile.mkdtemp())
    for name, text in INFO_FILES.items():
        p = os.path.join(base, name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            f.write(text)
    lvm_mod.link_vault_manager = types.SimpleNamespace(data={"creators": VAULT["creators"], "posts": VAULT["posts"]},
                                                       get_all_passwords=lambda: list(VAULT["all"]))
    bank = apm_mod.ArchivePasswordManager(os.path.join(base, "config"))
    steps = []
    for op, a in BANK_STEPS:
        a2 = {k: (v.replace("<B>", base).replace("/", os.sep) if isinstance(v, str) and "<B>" in v else v) for k, v in a.items()}
        if op == "add":
            r = bank.add_password(a2["password"], a2["creator"])
        elif op == "add_many":
            r = bank.add_passwords(a2["passwords"], a2["creator"])
        elif op == "remove":
            r = bank.remove_password(a2["password"], a2["creator"])
        elif op == "record":
            r = bank.record_archive_password(a2["archive"], a2["password"], a2["creator"])
        elif op == "get_archive":
            r = bank.get_archive_password(a2["archive"])
        elif op == "passwords":
            r = bank.get_passwords()
        elif op == "by_creator":
            r = bank.get_passwords_by_creator()
        elif op == "candidates":
            r = bank.find_candidate_passwords(a2["archive"], a2["creator"])
        elif op == "sync":
            r = bank.sync_from_link_vault()
        elif op == "clear":
            r = bank.clear_passwords()
        files = {}
        for fn in ("decompressor_passwords.json", "archive_password_map.json"):
            p = os.path.join(base, "config", fn)
            if os.path.exists(p):
                with open(p, encoding="utf-8") as fh:
                    files[fn] = rel(fh.read().replace("\\\\", "\\"), base.lower()).replace("\\", "/")
        steps.append({"op": op, "args": a, "result": rel(r, base), "files": files})
    out["bank"] = {"vault": VAULT, "info_files": INFO_FILES, "steps": steps}
    shutil.rmtree(base, ignore_errors=True)

    # ── Extraction with the bundled 7-Zip ──
    seven = bd.get_7za_path()
    base = os.path.realpath(tempfile.mkdtemp())
    archives = make_archives(seven, base)
    runs = []
    plan = [("extract", "plain.zip", None, {}), ("extract", "plain.zip", None, {}), ("extract", "locked.7z", None, {}),
            ("test", "locked.7z", "nope", {}), ("test", "locked.7z", "pw1", {}), ("extract", "locked.7z", "wrong", {}),
            ("extract", "locked.7z", "pw1", {}), ("probe", "locked.zip", None, {}), ("probe", "plain.zip", None, {}),
            ("extract", "locked.zip", "pw2", {"threads": 1}), ("extract", "broken.zip", None, {}),
            ("extract", "broken.zip", None, {"probe": False}), ("extract", "missing.zip", None, {})]
    for name, data in archives.items():
        with open(os.path.join(base, name), "wb") as f:
            f.write(base64.b64decode(data))
    known = {}
    for op, name, pw, extra in plan:
        path = os.path.join(base, name)
        if op == "test":
            r = eng.test_password(path, pw)
        elif op == "probe":
            r = list(eng.probe_if_encrypted(path))
        else:
            item = known.get(name) or bd.ArchiveItem(item_id=f"arc_{name}", path=path, filename=name, directory=base, size=0, creator="")
            known[name] = item
            progress = []
            ok, err = eng.extract_single_archive(item, password=pw, threads_per_archive=extra.get("threads", 2),
                                                 progress_callback=progress.append, probe=extra.get("probe", True))
            r = {"ok": ok, "error": err, "item": item.to_dict(), "last_progress": progress[-1] if progress else None}
        runs.append({"op": op, "name": name, "password": pw, "extra": extra, "result": rel(r, base), "files": files_of(base)})
    out["extract"] = {"archives": archives, "runs": rel(runs, base)}
    shutil.rmtree(base, ignore_errors=True)

    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    print(f"{len(NAMES)} names, {len(items)} scanned, {len(steps)} bank steps, {len(runs)} 7-Zip runs -> {FIXTURE}")
    for r in runs:
        print(r["op"], r["name"], r["password"], json.dumps(r["result"])[:200])


if __name__ == "__main__":
    main()
