# -*- mode: python ; coding: utf-8 -*-
"""
PyInstaller spec for the headless engine the desktop app starts (server/sidecar.py).

Output: dist/pawchive-engine/pawchive-engine.exe with its files in '_internal'.
It's a console program on purpose: the desktop shell talks to it through stdin / stdout and
starts it with no window, so nothing ever shows.
"""
import os

project_root = os.path.abspath(os.path.join(SPECPATH, ".."))

datas = [
    (os.path.join(project_root, "assets"), "assets"),
    (os.path.join(project_root, "locales"), "locales"),
    (os.path.join(project_root, "config", "settings.example.json"), "config"),
]

# Helper programs the engine runs (found next to the engine in 'dependencies/')
deps_src = os.path.join(project_root, "dependencies")
binaries = []
for name in ["7za.exe", "7z.exe", "7z.dll", "yt-dlp.exe"]:
    p = os.path.join(deps_src, name)
    if os.path.exists(p):
        binaries.append((p, "dependencies"))

hidden_imports = [
    "PySide6.QtCore", "PySide6.QtGui", "PySide6.QtWidgets", "PySide6.QtQml", "PySide6.QtQuick",
    "PySide6.QtNetwork", "PySide6.QtWebSockets", "PySide6.QtSvg",
    "Crypto", "Crypto.Cipher", "Crypto.Cipher.AES", "Crypto.Util", "Crypto.Util.Padding",
    "Crypto.Util.strxor", "Crypto.Random",
    "gdown", "requests", "urllib3", "brotli", "bs4", "psutil", "mutagen", "telethon", "qrcode",
    "PIL", "PIL.Image", "PIL.WebPImagePlugin", "PIL.AvifImagePlugin",
    "server", "server.sidecar", "server.media",
]
# Every engine module: much of it is imported lazily inside functions
for pkg in ("core", "bridge", "services"):
    for f in os.listdir(os.path.join(project_root, pkg)):
        if f.endswith(".py"):
            hidden_imports.append(f"{pkg}.{f[:-3]}" if f != "__init__.py" else pkg)

a = Analysis(
    [os.path.join(project_root, "server", "sidecar.py")],
    pathex=[project_root],
    binaries=binaries,
    datas=datas,
    hiddenimports=hidden_imports,
    excludes=["tkinter", "matplotlib", "scipy", "pandas", "unittest", "pytest", "IPython", "notebook",
              "PySide6.QtWebEngineCore", "PySide6.QtWebEngineWidgets", "PySide6.Qt3DCore", "PySide6.QtCharts",
              "PySide6.QtDataVisualization", "PySide6.QtPdf", "PySide6.QtDesigner"],
    noarchive=False,
)
# Never ship AI model files
a.datas = [d for d in a.datas if not (os.sep + "models" + os.sep in d[0] or d[0].lower().endswith((".onnx", ".gguf")))]

pyz = PYZ(a.pure)
exe = EXE(
    pyz, a.scripts, [],
    exclude_binaries=True,
    name="pawchive-engine",
    console=True,
    upx=False,
    icon=os.path.join(project_root, "assets", "icon.ico"),
    contents_directory="_internal",
)
coll = COLLECT(exe, a.binaries, a.datas, upx=False, name="pawchive-engine")
