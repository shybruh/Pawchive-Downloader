"""Writes the reference answers of the settings file (core/session_manager.py load_settings /
save_settings) for the Rust port's test (src-tauri/src/settings.rs).

    python tools/golden_settings.py

Loading: no file, a few settings, every kind of value, an unreadable file, a file that isn't an
object, a temporary download folder that's gone (or a test one), paths saved on Linux with a
"windows_drive_mount" pattern. Saving: the text written for a few settings (the login cookie never
written, non-ASCII kept, floats, nested values). Paths under the temporary folder are written as <B>,
the home folder as <HOME>.
Output: desktop/src-tauri/tests/fixtures/settings.json
"""

import json
import os
import shutil
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core import path_translation  # noqa: E402
from core.session_manager import SessionManager  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "settings.json")

LOADS = [
    ("no file", None),
    ("a few", {"threads": 8, "download_dir": "D:\\Kemono", "filter_type": "images"}),
    ("every kind", {"threads": 2, "bandwidth_limit_value": 2.5, "gallery_bookmarks": [{"name": "A", "path": "D:/x", "icon": "*"}],
                    "decompressor": {"max_parallel": 3, "custom_locations": [{"path": "E:\\a", "creator": "c"}]},
                    "skip_words": "a, b", "dark_theme": False, "extra_unknown": [1, "two", None], "cookie": "secret"}),
    ("unreadable", "{not json"),
    ("not an object", "[1, 2, 3]"),
    ("empty object", "{}"),
    ("temporary folder gone", {"download_dir": "<B>\\Temp\\gone", "threads": 3}),
    ("temporary folder there", {"download_dir": "<B>\\tmpkeep"}),
    ("test folder", {"download_dir": "C:\\x\\pawchive_tests_12\\dl"}),
    ("rebuild test folder", {"download_dir": "C:\\bridge_rebuild_test_9"}),
    ("linux paths", {"download_dir": "/mnt/d/Kemono", "storage_pool_drives": ["/mnt/e/pool", 5, {"path": "/mnt/f/x"}],
                     "gallery_bookmarks": [{"name": "B", "path": "/mnt/c/Users"}, "/mnt/g/plain", {"name": "no path"}]}),
    ("mount pattern", {"windows_drive_mount": "/media/me/{drive}", "download_dir": "/media/me/d/Kemono"}),
    ("utf-8 bom", "\ufeff{\"threads\": 6}"),
]
SAVES = [
    {"download_dir": "C:\\dl", "threads": 4, "cookie": "never", "page_end": 999999},
    {"name": "日本語 ✨", "f": 10.0, "g": 2.5, "h": 1e-07, "big": 12345678901234, "neg": -3, "t": True, "n": None,
     "nested": {"a": [], "b": {}, "c": [1, [2, {"d": "e"}]]}, "esc": "quote \" backslash \\ tab \t newline \n bell \u0007"},
    {},
]


def main():
    base = tempfile.mkdtemp(prefix="pawchive_settings_")
    home = os.path.expanduser("~")
    try:
        B = os.path.realpath(base)
        os.makedirs(os.path.join(B, "tmpkeep"))
        loads = []
        for name, content in LOADS:
            cfg = os.path.join(B, name.replace(" ", "_"))
            os.makedirs(cfg)
            path = os.path.join(cfg, "settings.json")
            if content is not None:
                text = content if isinstance(content, str) else json.dumps(content, ensure_ascii=False)
                with open(path, "w", encoding="utf-8") as f:
                    f.write(text.replace("<B>", B.replace("\\", "\\\\") if not isinstance(content, str) else B))
            path_translation.mount_pattern = "/mnt/{drive}"
            path_translation._found_mounts.clear()
            sm = SessionManager.__new__(SessionManager)
            sm.settings_file = path
            got = sm.load_settings()
            kept = sorted(f for f in os.listdir(cfg) if f != "settings.json")
            loads.append({"name": name, "file": (content if isinstance(content, str) or content is None
                                                 else json.dumps(content, ensure_ascii=False)),
                          "settings": got, "kept": [k.split("-")[0] for k in kept], "mount_pattern": path_translation.mount_pattern})
        path_translation.mount_pattern = "/mnt/{drive}"
        saves = []
        for i, data in enumerate(SAVES):
            sm = SessionManager.__new__(SessionManager)
            import threading
            sm._settings_lock = threading.Lock()
            sm.settings_file = os.path.join(B, f"save{i}", "settings.json")
            sm.save_settings(dict(data), silent=True)
            with open(sm.settings_file, encoding="utf-8", newline="") as f:   # (its line endings as written)
                saves.append({"data": data, "text": f.read()})
        out = {"home": home, "loads": loads, "saves": saves}
        text = json.dumps(out, ensure_ascii=False, indent=1)
        text = text.replace(json.dumps(B)[1:-1], "<B>").replace(json.dumps(home)[1:-1], "<HOME>")
    finally:
        shutil.rmtree(base, ignore_errors=True)
    with open(FIXTURE, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print(f"wrote {FIXTURE}: {len(LOADS)} loads, {len(SAVES)} saves")


if __name__ == "__main__":
    main()
