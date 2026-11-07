"""Headless engine for the Tauri desktop app.

Runs the same Python engine as main.py, without any QML window, and exposes every bridge object
over a local WebSocket using a small JSON protocol. Nothing is hand-wrapped: properties, slots and
signals are discovered through Qt's meta-object system, so the React UI can reach everything the
old QML UI could.

Started by the Tauri shell as:   python -m server.sidecar --port 0 --token <secret>
It prints one line to stdout once listening:   PAWCHIVE_SIDECAR_READY {"port": 51234}

Protocol (every message is a JSON object):
  client → server
    {"id": 1, "op": "describe"}                                  → every object, its props/methods/signals
    {"id": 2, "op": "get",  "obj": "appBridge", "prop": "currentUrl"}
    {"id": 3, "op": "set",  "obj": "appBridge", "prop": "currentUrl", "value": "https://…"}
    {"id": 4, "op": "call", "obj": "appBridge", "method": "startDownload", "args": []}
    {"id": 5, "op": "rows", "obj": "appBridge.queueModel", "start": 0, "count": 500}
  server → client
    {"id": 1, "ok": true, "result": …}   or   {"id": 1, "ok": false, "error": "…"}
    {"event": "props",  "obj": "appBridge", "values": {"currentUrl": "…", …}}
    {"event": "signal", "obj": "appBridge", "name": "showToast", "args": [...]}
    {"event": "model",  "obj": "appBridge.queueModel", "count": 12}     (rows changed: re-fetch)
"""
import argparse
import json
import os
import secrets
import sys
import threading
import time
import traceback

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)
os.chdir(ROOT)

from PySide6.QtCore import (  # noqa: E402
    QAbstractItemModel, QByteArray, QDate, QDateTime, QMetaMethod, QObject, QPointF, QRectF,
    QSizeF, QTime, QTimer, QUrl, Qt,
)
from PySide6.QtGui import QColor  # noqa: E402
from PySide6.QtNetwork import QHostAddress  # noqa: E402
from PySide6.QtWebSockets import QWebSocketServer  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402


# ── JSON conversion ─────────────────────────────────────────────────────────────────────────────

class Registry:
    """Names every exposed QObject. Child QObjects reached through a property get a dotted name."""

    def __init__(self):
        self.by_name = {}
        self.names = {}

    def add(self, name, obj):
        if obj is None:
            return
        self.by_name[name] = obj
        self.names[id(obj)] = name

    def name_of(self, obj):
        return self.names.get(id(obj))


def to_json(value, registry=None, depth=0):
    if depth > 12:
        return None
    if value is None or isinstance(value, (bool, int, str)):
        return value
    if isinstance(value, float):
        return value if value == value and value not in (float("inf"), float("-inf")) else None
    if isinstance(value, dict):
        return {str(k): to_json(v, registry, depth + 1) for k, v in value.items()}
    if isinstance(value, (list, tuple, set)):
        return [to_json(v, registry, depth + 1) for v in value]
    if isinstance(value, QUrl):
        return value.toString()
    if isinstance(value, QColor):
        return value.name(QColor.HexArgb) if value.alpha() < 255 else value.name()
    if isinstance(value, QByteArray):
        return bytes(value).decode("utf-8", "replace")
    if isinstance(value, (QDateTime, QDate, QTime)):
        return value.toString(Qt.ISODate)
    if isinstance(value, QPointF):
        return {"x": value.x(), "y": value.y()}
    if isinstance(value, QSizeF):
        return {"width": value.width(), "height": value.height()}
    if isinstance(value, QRectF):
        return {"x": value.x(), "y": value.y(), "width": value.width(), "height": value.height()}
    if isinstance(value, QObject):
        name = registry.name_of(value) if registry else None
        return {"$ref": name} if name else None
    if isinstance(value, bytes):
        return value.decode("utf-8", "replace")
    try:
        json.dumps(value)
        return value
    except Exception:
        return str(value)


# ── Reflection of one QObject ──────────────────────────────────────────────────────────────────

_BASE_PROPS = {"objectName"}
HEAVY_PROPS = {"galleryBookmarks"}
HEAVY_INTERVAL = 0.5
# Getters that are slow (ffmpeg probes, disk checks) or return large snapshots the UI reads another
# way: never read up front or pushed; a client fetches them with "get" when it needs them.
LAZY_PROPS = {
    "detectedHwAccel", "availableHwAccels", "itemsJson", "groupedItemsJson", "passwordBankTreeJson",
    "linkVaultTreeJson", "harvestedLinks", "storagePoolStatusJson",
}


QT_BASES = {"QObject", "QAbstractItemModel", "QAbstractListModel", "QAbstractTableModel",
            "QStandardItemModel", "QSortFilterProxyModel", "QIdentityProxyModel"}


def _qt_base(meta):
    """The nearest Qt (C++) class in a meta-object's chain."""
    m = meta
    while m is not None and m.className() not in QT_BASES:
        m = m.superClass()
    return m if m is not None else QObject.staticMetaObject


class Exposed:
    def __init__(self, name, obj, hub):
        self.name = name
        self.obj = obj
        self.hub = hub
        meta = obj.metaObject()
        self.props = {}          # prop name -> {type, writable}
        self.signal_props = {}   # signal name -> [prop names it notifies]
        self.methods = {}        # slot / invokable name -> param names
        self.signals = {}        # signal name -> param names

        for i in range(meta.propertyCount()):
            p = meta.property(i)
            pname = p.name()
            if pname in _BASE_PROPS:
                continue
            self.props[pname] = {"type": p.typeName(), "writable": p.isWritable()}
            if p.hasNotifySignal():
                sig = bytes(p.notifySignal().name()).decode()
                self.signal_props.setdefault(sig, []).append(pname)

        # Start after the Qt base class's own methods (QObject, QAbstractListModel…): PySide can
        # split a Python class over several meta-object layers, so methodOffset() alone would miss
        # the earlier ones, and the base's methods (dataChanged, rowsInserted…) aren't the engine's
        for i in range(_qt_base(meta).methodCount(), meta.methodCount()):
            m = meta.method(i)
            mname = bytes(m.name()).decode()
            params = [bytes(n).decode() for n in m.parameterNames()]
            if m.methodType() == QMetaMethod.Signal:
                self.signals[mname] = params
            elif m.methodType() in (QMetaMethod.Slot, QMetaMethod.Method):
                if mname.startswith("_"):
                    continue
                # Overloads: keep the longest parameter list (optional args are filled by Python)
                if len(params) >= len(self.methods.get(mname, [])):
                    self.methods[mname] = params

        self.is_model = isinstance(obj, QAbstractItemModel)
        self._connect()

    def _connect(self):
        for sig in self.signals:
            try:
                bound = getattr(self.obj, sig)
            except AttributeError:
                continue
            props = self.signal_props.get(sig)
            if props:
                bound.connect(lambda *a, _p=tuple(props): self.hub.mark_dirty(self.name, _p))
            elif not sig.startswith("_"):      # "_x" signals are the engine's internal plumbing
                bound.connect(lambda *a, _s=sig: self.hub.queue_signal(self.name, _s, a))
        if self.is_model:
            m = self.obj
            for sig in (m.modelReset, m.rowsInserted, m.rowsRemoved, m.rowsMoved, m.dataChanged, m.layoutChanged):
                sig.connect(lambda *a: self.hub.mark_model(self.name))

    def read(self, prop):
        return self.obj.property(prop)

    def describe(self, registry):
        values = {}
        for p in self.props:
            if p in LAZY_PROPS:
                continue
            try:
                values[p] = to_json(self.read(p), registry)
            except Exception:
                values[p] = None
        out = {
            "props": {k: dict(v, value=values.get(k), lazy=k in LAZY_PROPS) for k, v in self.props.items()},
            "methods": self.methods,
            "signals": [s for s in self.signals if s not in self.signal_props and not s.startswith("_")],
        }
        if self.is_model:
            out["model"] = {"count": self.obj.rowCount(), "roles": self.role_names()}
        return out

    def role_names(self):
        return {int(k): bytes(v).decode() for k, v in self.obj.roleNames().items()}

    def rows(self, start, count, registry):
        m = self.obj
        roles = self.role_names()
        total = m.rowCount()
        start = max(0, int(start or 0))
        end = total if count is None else min(total, start + int(count))
        out = []
        for r in range(start, end):
            idx = m.index(r, 0)
            out.append({name: to_json(m.data(idx, role), registry) for role, name in roles.items()})
        return {"total": total, "start": start, "rows": out}


# ── The WebSocket hub ──────────────────────────────────────────────────────────────────────────

class Hub(QObject):
    def __init__(self, token, port, registry):
        super().__init__()
        self.token = token
        self.registry = registry
        self.exposed = {}
        self.clients = []
        self._lock = threading.Lock()
        self._dirty = {}            # obj -> set(props)
        self._signals = []          # (obj, name, args)
        self._models = set()
        self._heavy_sent = {}       # (obj, prop) -> monotonic time of the last push
        self.media_port = 0

        self.server = QWebSocketServer("pawchive-sidecar", QWebSocketServer.NonSecureMode, self)
        if not self.server.listen(QHostAddress.LocalHost, port):
            raise RuntimeError(self.server.errorString())
        self.server.newConnection.connect(self._on_connection)

        self.flush_timer = QTimer(self)
        self.flush_timer.setInterval(40)
        self.flush_timer.timeout.connect(self._flush)
        self.flush_timer.start()

    @property
    def port(self):
        return self.server.serverPort()

    # Exposure (walks QObject-valued properties so models and sub-bridges are reachable)
    def expose(self, name, obj):
        if obj is None or name in self.exposed:
            return
        existing = self.registry.name_of(obj)
        if existing and existing in self.exposed:
            # Same object reached under a second name (e.g. appBridge.decompressorBridge):
            # an alias, so it's reflected, read and pushed only once, under its first name
            self.exposed[name] = self.exposed[existing]
            self.registry.by_name[name] = obj
            return
        self.registry.add(name, obj)
        ex = Exposed(name, obj, self)
        self.exposed[name] = ex
        for pname, info in ex.props.items():
            t = info["type"] or ""
            if t.endswith("*") or "QObject" in t or "Model" in t:
                try:
                    child = ex.read(pname)
                except Exception:
                    child = None
                if isinstance(child, QObject):
                    self.expose(f"{name}.{pname}", child)

    # Called from any thread
    def mark_dirty(self, obj, props):
        with self._lock:
            self._dirty.setdefault(obj, set()).update(props)

    def queue_signal(self, obj, name, args):
        with self._lock:
            self._signals.append((obj, name, args))

    def mark_model(self, obj):
        with self._lock:
            self._models.add(obj)

    def _flush(self):
        with self._lock:
            dirty, self._dirty = self._dirty, {}
            sigs, self._signals = self._signals, []
            models, self._models = self._models, set()
        if not self.clients:
            return
        now = time.monotonic()
        for obj, props in dirty.items():
            ex = self.exposed.get(obj)
            if not ex:
                continue
            values = {}
            for p in props:
                if p in LAZY_PROPS:
                    continue
                # Big JSON snapshots (decompressor items, vault tree…) change in bursts: push them at
                # most twice a second so serialising them can't hog the engine's main thread
                if p.endswith("Json") or p in HEAVY_PROPS:
                    last = self._heavy_sent.get((obj, p), 0.0)
                    if now - last < HEAVY_INTERVAL:
                        self.mark_dirty(obj, (p,))
                        continue
                    self._heavy_sent[(obj, p)] = now
                try:
                    v = ex.read(p)
                except Exception:
                    continue
                if isinstance(v, QObject) and not self.registry.name_of(v):
                    self.expose(f"{obj}.{p}", v)
                values[p] = to_json(v, self.registry)
            self._broadcast({"event": "props", "obj": obj, "values": values})
        for obj, name, args in sigs:
            self._broadcast({"event": "signal", "obj": obj, "name": name, "args": to_json(list(args), self.registry)})
        for obj in models:
            ex = self.exposed.get(obj)
            if ex:
                self._broadcast({"event": "model", "obj": obj, "count": ex.obj.rowCount()})

    def _broadcast(self, msg):
        data = json.dumps(msg, ensure_ascii=False)
        for c in list(self.clients):
            c.sendTextMessage(data)

    def _on_connection(self):
        sock = self.server.nextPendingConnection()
        query = sock.requestUrl().query()
        if f"token={self.token}" not in query.split("&"):
            sock.close()
            return
        self.clients.append(sock)
        sock.textMessageReceived.connect(lambda text, s=sock: self._on_message(s, text))
        sock.disconnected.connect(lambda s=sock: self.clients.remove(s) if s in self.clients else None)

    def _on_message(self, sock, text):
        try:
            msg = json.loads(text)
        except ValueError:
            return
        mid = msg.get("id")
        try:
            result = self._handle(msg)
            reply = {"id": mid, "ok": True, "result": result}
        except Exception as e:  # report back, keep serving
            reply = {"id": mid, "ok": False, "error": f"{type(e).__name__}: {e}"}
            if os.environ.get("PAWCHIVE_SIDECAR_DEBUG"):
                traceback.print_exc()
        sock.sendTextMessage(json.dumps(reply, ensure_ascii=False))

    def _get(self, name):
        ex = self.exposed.get(name)
        if not ex:
            raise KeyError(f"unknown object {name!r}")
        return ex

    def _handle(self, msg):
        op = msg.get("op")
        if op == "describe":
            return {name: ex.describe(self.registry) for name, ex in self.exposed.items() if ex.name == name}
        if op == "info":
            return {"mediaPort": self.media_port}
        if op == "get":
            return to_json(self._get(msg["obj"]).read(msg["prop"]), self.registry)
        if op == "set":
            ex = self._get(msg["obj"])
            if not ex.props.get(msg["prop"], {}).get("writable"):
                raise PermissionError(f"{msg['prop']} is read-only")
            ex.obj.setProperty(msg["prop"], msg.get("value"))
            return to_json(ex.read(msg["prop"]), self.registry)
        if op == "call":
            ex = self._get(msg["obj"])
            method = msg["method"]
            if method not in ex.methods:
                raise AttributeError(f"{msg['obj']} has no method {method!r}")
            args = [self._resolve(a) for a in (msg.get("args") or [])]
            return to_json(getattr(ex.obj, method)(*args), self.registry)
        if op == "rows":
            ex = self._get(msg["obj"])
            if not ex.is_model:
                raise TypeError(f"{msg['obj']} is not a list model")
            return ex.rows(msg.get("start", 0), msg.get("count"), self.registry)
        raise ValueError(f"unknown op {op!r}")

    def _resolve(self, arg):
        if isinstance(arg, dict) and set(arg) == {"$ref"}:
            return self._get(arg["$ref"]).obj
        return arg


# ── Engine start-up (mirrors main.py, minus the QML window) ────────────────────────────────────

def build_engine():
    from bridge.app_bridge import AppBridge
    from bridge.scheduler_bridge import SchedulerBridge
    from bridge.gallery_tools_bridge import GalleryToolsBridge
    from bridge.gallery_updates import GalleryUpdates

    app_bridge = AppBridge()
    app_bridge.archive_manager.start_maintenance()

    wl = getattr(app_bridge, "_watchlist_manager", None)
    tools = GalleryToolsBridge(app_bridge, wl)
    # Sub-bridges first, so they keep their short names when appBridge's properties reach them again
    objects = {
        "decompressorBridge": app_bridge.decompressorBridge,
        "telegramBridge": app_bridge.telegramBridge,
        "appBridge": app_bridge,
        "scheduler": SchedulerBridge(app_bridge),
        "linkVault": app_bridge.link_vault,
        "galleryTools": tools,
        "galleryUpdates": GalleryUpdates(app_bridge, wl),
    }
    return app_bridge, objects


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--token", default=os.environ.get("PAWCHIVE_SIDECAR_TOKEN") or secrets.token_hex(16))
    args = ap.parse_args()

    # Engine widgets (native file pickers etc.) still need a QApplication, but no window is shown
    app = QApplication(sys.argv[:1])
    app.setApplicationName("PawchiveDownloader")
    app.setOrganizationName("PawchiveProject")
    app.setQuitOnLastWindowClosed(False)

    from core.logger import logger
    # Write this run's log file (logs/v<version>/<date_time>.log); crash logs and "Open logs" use it
    logger.start_session()
    app_bridge, objects = build_engine()

    registry = Registry()
    hub = Hub(args.token, args.port, registry)
    for name, obj in objects.items():
        hub.expose(name, obj)

    # Thumbnails, pictures and files for the UI are served by the desktop shell (Rust), not here

    from services.telegram_service import TelegramService

    def _arm_exit_watchdog():
        t = threading.Timer(20.0, lambda: os._exit(0))
        t.daemon = True
        t.start()

    app.aboutToQuit.connect(app_bridge.onAppClosing)
    app.aboutToQuit.connect(_arm_exit_watchdog)
    app.aboutToQuit.connect(TelegramService.instance().stop)

    # The shell closes our stdin when it exits: treat that as "quit" so no engine is left behind
    def _watch_stdin():
        try:
            while sys.stdin.read(1):
                pass
        except Exception:
            pass
        QTimer.singleShot(0, app.quit)

    if not sys.stdin.isatty():
        threading.Thread(target=_watch_stdin, name="stdin-watch", daemon=True).start()

    logger.success("Engine ready (headless sidecar).", category="system")
    from core.path_utils import get_config_dir, get_data_dir
    # The shell's Rust parts read and write the same files
    print("PAWCHIVE_SIDECAR_READY " + json.dumps({
        "port": hub.port, "objects": len(hub.exposed),
        "dataDir": os.path.join(get_data_dir(), "data"), "configDir": get_config_dir(),
    }), flush=True)
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
