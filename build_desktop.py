"""Builds the desktop app for distribution.

    python build_desktop.py            # engine + app, portable zip and installer
    python build_desktop.py --no-engine  # reuse an engine built earlier
    python build_desktop.py --no-installer  # portable zip only

Everything lands in dist/ (ignored by git):
  dist/pawchive-engine/                   the engine (PyInstaller)
  dist/Pawchive Downloader/               portable app: the .exe plus its engine/ folder
  dist/Pawchive-Downloader-<ver>-portable.zip
  dist/Pawchive-Downloader-<ver>-setup.exe
"""
import argparse
import json
import os
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.abspath(__file__))
DESKTOP = os.path.join(ROOT, "desktop")
TAURI = os.path.join(DESKTOP, "src-tauri")
DIST = os.path.join(ROOT, "dist")
ENGINE_OUT = os.path.join(DIST, "pawchive-engine")


def run(cmd, cwd=ROOT):
    print(f"\n> {' '.join(cmd)}", flush=True)
    subprocess.run(cmd, cwd=cwd, check=True, shell=(os.name == "nt" and cmd[0] in ("npm", "npx")))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--no-engine", action="store_true")
    ap.add_argument("--no-installer", action="store_true", help="portable build only (skip the NSIS installer)")
    args = ap.parse_args()

    conf = json.load(open(os.path.join(TAURI, "tauri.conf.json"), encoding="utf-8"))
    version = conf["version"]
    product = conf["productName"]

    # 1. Engine
    if not args.no_engine:
        run([sys.executable, "-m", "PyInstaller", "--noconfirm", "--clean",
             "--distpath", DIST, "--workpath", os.path.join(ROOT, "build", "engine"),
             os.path.join(ROOT, "server", "engine.spec")])
    if not os.path.isfile(os.path.join(ENGINE_OUT, "pawchive-engine.exe")):
        sys.exit("The engine isn't built (dist/pawchive-engine).")
    # The engine reads its version from next to its .exe
    with open(os.path.join(ROOT, "version.json"), encoding="utf-8") as f:
        info = json.load(f)
    info["version"] = version
    with open(os.path.join(ENGINE_OUT, "version.json"), "w", encoding="utf-8") as f:
        json.dump(info, f, indent=4)

    # 2. App + installer. The engine goes in as a resource only for this build, so day-to-day
    #    development doesn't need it.
    release_conf = os.path.join(ROOT, "build", "tauri.release.json")
    os.makedirs(os.path.dirname(release_conf), exist_ok=True)
    json.dump({"bundle": {"targets": ["nsis"], "resources": {ENGINE_OUT.replace("\\", "/") + "/": "engine/"}}},
              open(release_conf, "w"))
    if args.no_installer:
        run(["npm", "run", "tauri", "build", "--", "--no-bundle"], cwd=DESKTOP)
    else:
        run(["npm", "run", "tauri", "build", "--", "--config", release_conf], cwd=DESKTOP)

    # 3. Portable folder + zip
    portable = os.path.join(DIST, product)
    shutil.rmtree(portable, ignore_errors=True)
    os.makedirs(portable)
    shutil.copy2(os.path.join(TAURI, "target", "release", "pawchive.exe"), os.path.join(portable, f"{product}.exe"))
    shutil.copytree(ENGINE_OUT, os.path.join(portable, "engine"))
    shutil.copy2(os.path.join(ROOT, "LICENSE"), os.path.join(portable, "LICENSE.txt"))
    # Keeps settings and data next to the app instead of the user's AppData folder
    with open(os.path.join(portable, "portable"), "w", encoding="utf-8") as f:
        f.write("Settings, data and logs are kept in this folder. Delete this file to use AppData instead.
")
    base = os.path.join(DIST, f"{product.replace(' ', '-')}-{version}-portable")
    shutil.make_archive(base, "zip", DIST, product)

    nsis_dir = os.path.join(TAURI, "target", "release", "bundle", "nsis")
    for f in os.listdir(nsis_dir) if os.path.isdir(nsis_dir) else []:
        if f.endswith(".exe"):
            shutil.copy2(os.path.join(nsis_dir, f), os.path.join(DIST, f"{product.replace(' ', '-')}-{version}-setup.exe"))

    print(f"\nDone. Outputs are in {DIST}")


if __name__ == "__main__":
    main()
