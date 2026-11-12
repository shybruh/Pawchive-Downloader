"""Writes the reference answers of the Python Link Vault (core/link_vault_manager.py) for the Rust
port's test (src-tauri/src/vault.rs).

    python tools/golden_vault.py

A run of vault operations on a fresh vault, with made-up ids ("id-1", "id-2"…, uuid4's place) and
a set clock per step: each step's answer, the vault after it, and the database rows at the end
(the Rust vault must also read the Python one's database). The export's files too.
Output: desktop/src-tauri/tests/fixtures/vault.json
"""

import datetime as real_dt
import json
import os
import shutil
import sqlite3
import sys
import tempfile
import types

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from core import link_vault_manager as lvm  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "vault.json")

CLOCK = {"t": real_dt.datetime(2026, 1, 1, 12, 0, 0)}
IDS = {"n": 0}


class _DT(real_dt.datetime):
    @classmethod
    def now(cls, tz=None):
        return CLOCK["t"]


def _uuid4():
    IDS["n"] += 1
    return f"id-{IDS['n']}-0000-0000-0000-000000000000"


lvm.datetime = types.SimpleNamespace(datetime=_DT)
lvm.uuid = types.SimpleNamespace(uuid4=_uuid4)

P1 = {"post_id": "101", "title": "  Pack <one>  ", "published": "2026-01-05", "content": "<p>Get it<br/>here &amp; now</p><li>a",
      "post_url": "https://x/post/101", "passwords": ["pw1", "pw1", "pw2"],
      "links": ["https://mega.nz/file/AAA#k/", {"url": "https://drive.google.com/x", "platform": "gdrive", "passwords": ["dpw"],
                                                "password_source": "near"},
                {"url": "  ", "platform": "other"}, 42, {"url": "https://gofile.io/d/zz", "platform": "gofile"}]}
P2 = {"title": "", "date": "2026-02-01", "description": "Plain text\n\n\n\nmore", "url": "https://x/p2",
      "links": [{"url": "https://pixeldrain.com/u/abc", "platform": "pixeldrain", "passwords": []}]}
P3 = {"id": 103, "title": "Third", "published": "2025-12-30", "full_text": "Password: secret",
      "links": ["https://mega.nz/file/AAA#k", {"url": "https://catbox.moe/c/f.zip", "platform": "catbox", "passwords": ["zz", "aa"]}],
      "passwords": ["pwX"]}
Q1 = {"post_id": "201", "title": "Other creator", "published": "2026-03-01", "content": "",
      "links": [{"url": "https://mega.nz/folder/BBB#k", "platform": "mega", "passwords": ["m1"]}, "https://dropbox.com/s/q"]}

STEPS = [
    ["2026-01-01T12:00:00", "harvest", {"creator_name": "  Artist A ", "service": "Patreon", "user_id": " 123 ", "posts": [P1, P2]}],
    ["2026-01-02T08:00:00", "harvest", {"creator_name": "Model B", "service": "onlyfans", "user_id": "77", "posts": [Q1]}],
    ["2026-01-03T09:30:00", "harvest", {"creator_name": "Artist A2", "service": "patreon", "user_id": "123", "posts": [P3]}],
    ["2026-01-03T09:31:00", "harvest", {"creator_name": "", "service": "", "user_id": "", "posts": [{"links": ["https://x.y/z"]}]}],
    ["2026-01-03T09:32:00", "harvest", {"creator_name": "Nobody", "service": "x", "user_id": "1", "posts": []}],
    ["2026-01-04T10:00:00", "tree", {"search": "", "platform": ""}],
    ["2026-01-04T10:00:00", "tree", {"search": "SECRET", "platform": ""}],
    ["2026-01-04T10:00:00", "tree", {"search": "", "platform": "MEGA"}],
    ["2026-01-04T10:00:00", "tree", {"search": "model", "platform": "mega"}],
    ["2026-01-04T10:00:00", "tree", {"search": "dpw", "platform": "all"}],
    ["2026-01-04T10:00:00", "tree", {"search": "zzzz", "platform": ""}],
    ["2026-01-05T10:00:00", "passwords_set", {"id": "id-3-0000-0000-0000-000000000000", "passwords": [" b ", "a", "", "b", 7]}],
    ["2026-01-05T10:00:00", "passwords_set", {"id": "nope", "passwords": ["x"]}],
    ["2026-01-05T11:00:00", "health", {"updates": [["id-2-0000-0000-0000-000000000000", "dead"], ["id-4-0000-0000-0000-000000000000", "alive"],
                                                  ["missing", "dead"]]}],
    ["2026-01-05T11:00:00", "all_passwords", {}],
    ["2026-01-05T11:00:00", "urls", {"creator": ""}],
    ["2026-01-05T11:00:00", "urls", {"creator": " ONLYFANS:77 "}],
    ["2026-01-06T09:00:00", "export", {}],
    ["2026-01-06T10:00:00", "clean_dead", {}],
    ["2026-01-06T10:00:00", "clean_dead", {}],
    ["2026-01-06T10:00:00", "delete_link", {"id": "id-4-0000-0000-0000-000000000000"}],
    ["2026-01-06T10:00:00", "delete_link", {"id": "nope"}],
    ["2026-01-06T10:00:00", "delete_post", {"id": "201"}],
    ["2026-01-06T10:00:00", "delete_post", {"id": "nope"}],
    ["2026-01-06T10:00:00", "delete_creator", {"key": "PATREON:123"}],
    ["2026-01-06T10:00:00", "delete_creator", {"key": "nope"}],
    ["2026-01-06T10:00:00", "tree", {"search": "", "platform": ""}],
]


def run(vm, op, a, export_dir):
    if op == "harvest":
        return vm.add_harvested_data(a["creator_name"], a["service"], a["user_id"], a["posts"])
    if op == "tree":
        return vm.get_tree_model(a["search"], a["platform"])
    if op == "passwords_set":
        return vm.update_link_passwords(a["id"], a["passwords"])
    if op == "health":
        for lid, h in a["updates"]:
            vm.update_link_health(lid, h, save=False)
        vm.save()
        return None
    if op == "all_passwords":
        return vm.get_all_passwords()
    if op == "urls":
        return vm.get_all_link_urls(a["creator"])
    if op == "export":
        n = vm.export_all_to_folder(export_dir)
        files = {}
        for f in sorted(os.listdir(export_dir)):
            with open(os.path.join(export_dir, f), encoding="utf-8") as fh:
                files[f] = fh.read()
        return {"count": n, "files": files}
    if op == "clean_dead":
        return vm.clean_dead_links()
    if op == "delete_link":
        return vm.delete_link(a["id"])
    if op == "delete_post":
        return vm.delete_post(a["id"])
    if op == "delete_creator":
        return vm.delete_creator(a["key"])
    raise ValueError(op)


def rows(path):
    conn = sqlite3.connect(path)
    out = {"creators": [[k, json.loads(d)] for k, d in conn.execute("SELECT key, data FROM creators ORDER BY key;")],
           "posts": [[k, s, json.loads(d)] for k, s, d in conn.execute("SELECT post_id, seq, data FROM posts ORDER BY post_id;")],
           "links": [[k, s, json.loads(d)] for k, s, d in conn.execute("SELECT id, seq, data FROM links ORDER BY id;")]}
    conn.close()
    return out


def main():
    base = tempfile.mkdtemp()
    vm = lvm.LinkVaultManager(base)
    out = []
    for when, op, a in STEPS:
        CLOCK["t"] = real_dt.datetime.fromisoformat(when)
        export_dir = os.path.join(base, "export")
        result = json.loads(json.dumps(run(vm, op, a, export_dir)))     # (as it was then: the tree shares the vault's links)
        out.append({"time": when, "op": op, "args": a, "result": result, "data": json.loads(json.dumps(vm.data))})
    db = rows(vm.store.path)
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump({"steps": out, "rows": db}, fh, ensure_ascii=False, indent=1)
    shutil.rmtree(base, ignore_errors=True)
    print(f"{len(out)} steps -> {FIXTURE}")
    for s in out:
        r = json.dumps(s["result"], ensure_ascii=False)
        print(s["op"], r[:150])


if __name__ == "__main__":
    main()
