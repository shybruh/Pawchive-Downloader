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
    "PySide6.QtCore", "PySide6.QtGui", "PySide6.QtWidgets", "PySide6.QtNetwork", "PySide6.QtWebSockets",
    "Crypto", "Crypto.Cipher", "Crypto.Cipher.AES", "Crypto.Util", "Crypto.Util.Padding",
    "Crypto.Util.strxor", "Crypto.Random",
    "gdown", "requests", "urllib3", "brotli", "bs4", "psutil", "mutagen", "telethon", "qrcode",
    "PIL", "PIL.Image", "PIL.WebPImagePlugin", "PIL.AvifImagePlugin",
    "server", "server.sidecar",
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
    # The engine only uses QtCore, QtGui, QtWidgets, QtNetwork and QtWebSockets; everything else in
    # PySide6 (Quick / QML, multimedia, web engine, 3D…) is left out, which is most of its size
    excludes=["tkinter", "matplotlib", "scipy", "pandas", "unittest", "pytest", "IPython", "notebook", "numpy"] + [
        f"PySide6.{m}" for m in (
            "QtQml", "QtQuick", "QtQuickControls2", "QtQuickWidgets", "QtQuick3D", "QtQuickTest", "QtMultimedia",
            "QtMultimediaWidgets", "QtSpatialAudio", "QtWebEngineCore", "QtWebEngineWidgets", "QtWebEngineQuick",
            "QtWebChannel", "QtWebView", "Qt3DCore", "Qt3DRender", "Qt3DInput", "Qt3DLogic", "Qt3DAnimation",
            "Qt3DExtras", "QtCharts", "QtGraphs", "QtDataVisualization", "QtPdf", "QtPdfWidgets", "QtDesigner",
            "QtHelp", "QtUiTools", "QtSql", "QtTest", "QtOpenGL", "QtOpenGLWidgets", "QtSvg", "QtSvgWidgets",
            "QtPrintSupport", "QtVirtualKeyboard", "QtShaderTools", "QtLocation", "QtPositioning", "QtSensors",
            "QtBluetooth", "QtNfc", "QtSerialPort", "QtSerialBus", "QtRemoteObjects", "QtScxml",
            "QtStateMachine", "QtTextToSpeech", "QtHttpServer", "QtConcurrent", "QtXml", "QtDBus",
            "QtAxContainer", "QtCanvasPainter",
        )
    ],
    noarchive=False,
)
# Never ship AI model files
a.datas = [d for d in a.datas if not (os.sep + "models" + os.sep in d[0] or d[0].lower().endswith((".onnx", ".gguf")))]

# Qt plugins drag in libraries a windowless engine never loads (Quick / QML through the virtual
# keyboard, PDF through its image plugin, software OpenGL); drop them
_DROP = ("qt6qml", "qt6quick", "qt6pdf", "qt6virtualkeyboard", "opengl32sw", "qvirtualkeyboard", "qpdf",
         "qt6opengl", "qt6svg", "qsvg")
a.binaries = [b for b in a.binaries if not any(k in os.path.basename(b[0]).lower() for k in _DROP)]
a.datas = [d for d in a.datas if "PySide6" not in d[0].replace("\\", "/").split("/")[0] or "translations" not in d[0]]

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
