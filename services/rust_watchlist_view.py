"""
The window's watchlist (filtering, order, counts, rows, which artists are on disk) answered by the
desktop shell's Rust port (desktop/src-tauri/src/watchlist/view.rs, served by
src-tauri/src/host/watchlist.rs as "appBridge.watchlistModel").

RustWatchlistModel is the app's WatchlistModel with its artists mirrored to Rust: the bridge
still refreshes it as before, and every change to an artist (a check's result, a new date, a
folder) goes over a moment later. The window's filter is set in Rust; toggling a favorite is
passed back here (it changes the watchlist).
"""

import itertools
import threading
import time
from typing import Any, Dict, List, Optional

from PySide6.QtCore import Qt, Signal, Slot

from bridge.watchlist_model import WatchlistModel
from core.logger import logger
from services import rust_queue

_keys = itertools.count(1)


def available() -> bool:
    return rust_queue.available()


def _key(e) -> str:
    k = e.__dict__.get("_vkey")
    if not k:
        k = f"w{next(_keys)}"
        object.__setattr__(e, "_vkey", k)        # (not a watchlist field: not saved, no change hook)
    return k


def _light(f) -> Any:
    """A found post's file, as far as the list needs it"""
    if isinstance(f, dict):
        return {k: f[k] for k in ("name", "path", "storageKey") if k in f}
    return f


def _light_post(p: dict) -> dict:
    out = {k: p[k] for k in ("id", "title", "captionText", "caption", "published", "added") if k in p}
    if "file" in p:
        out["file"] = _light(p["file"])
    if isinstance(p.get("attachments"), list):
        out["attachments"] = [_light(a) for a in p["attachments"]]
    return out


def row_of(e) -> Dict[str, Any]:
    """An artist as Rust's view takes it (view.rs Row)."""
    return {
        "key": _key(e), "url": e.url or "", "creator_name": e.creator_name or "", "service": e.service or "",
        "domain": e.domain or "", "user_id": e.user_id or "", "last_post_date": e.last_post_date or "",
        "last_post_id": e.last_post_id or "", "added_at": e.added_at or "", "auto_check": bool(e.auto_check),
        "new_post_count": int(e.new_post_count or 0), "download_dir": e.download_dir or "",
        "download_dirs": [d for d in (e.download_dirs or []) if isinstance(d, str)],
        "ignored_count": len(getattr(e, "ignored_post_ids", None) or []),
        "posts": [_light_post(p) for p in (getattr(e, "cached_new_posts", None) or []) if isinstance(p, dict)],
        "is_favorite": bool(getattr(e, "is_favorite", False)),
    }


class RustWatchlistModel(WatchlistModel):
    _guiCall = Signal(object)

    def __init__(self, watchlist_manager, parent=None):
        self._sent: Dict[str, Dict[str, Any]] = {}
        self._sent_order: Optional[List[str]] = None
        self._sent_meta: Optional[Dict[str, Any]] = None
        self._sent_on = None
        self._full = True
        self._dirty: Dict[int, Any] = {}
        self._lock = threading.Lock()
        self._wake = threading.Event()
        super().__init__(watchlist_manager, parent)
        self._guiCall.connect(self._run_gui_call, Qt.QueuedConnection)
        listeners = getattr(watchlist_manager, "change_listeners", None)
        if listeners is not None:
            listeners.append(self._entry_dirty)
        link = rust_queue.shared_link()
        link.handlers["watchlist_call"] = self._window_call
        link.on_connect.append(self.refresh)
        threading.Thread(target=self._flush_loop, name="rust-watchlist-view", daemon=True).start()
        self._wake.set()

    # ── Changes go to Rust ────────────────────────────────────────────────────

    def refresh(self):
        super().refresh()
        self._full = True
        self._wake.set()

    def entry_changed(self, entry):
        super().entry_changed(entry)
        self._entry_dirty(entry)

    def _entry_dirty(self, entry):
        with self._lock:
            self._dirty[id(entry)] = entry
        self._wake.set()

    def _flush_loop(self):
        while True:
            self._wake.wait()
            time.sleep(0.15)
            self._wake.clear()
            try:
                self._push()
            except Exception as e:
                logger.debug(f"Couldn't update the watchlist view: {e}", category="watchlist")

    def _meta(self) -> Dict[str, Any]:
        from core import folder_naming
        return {"base_dir": self._get_base_dir() or "", "include_service": bool(folder_naming.include_service)}

    def _push(self):
        link = rust_queue.shared_link()
        if not link.connect():
            return
        if self._sent_on is not link._sock:
            self._sent_on = link._sock
            self._sent, self._sent_order, self._sent_meta = {}, None, None
            self._full = True
        with self._lock:
            full, self._full = self._full, False
            dirty, self._dirty = self._dirty, {}
        if full:
            lock = getattr(self._manager, "_lock", None)
            if lock is not None:
                with lock:
                    entries = list(self._manager.entries)
            else:
                entries = list(self._manager.entries)
        else:
            entries = list(dirty.values())
        rows = []
        for e in entries:
            r = row_of(e)
            if self._sent.get(r["key"]) != r:
                self._sent[r["key"]] = r
                rows.append(r)
        order = None
        if full:
            order = [_key(e) for e in entries]
            keep = set(order)
            for k in [k for k in self._sent if k not in keep]:
                del self._sent[k]
            if order == self._sent_order:
                order = None
            else:
                self._sent_order = order
        meta = self._meta()
        if not rows and order is None and meta == self._sent_meta and not full:
            return
        self._sent_meta = meta
        link.send({"op": "watchlist_view", "order": order, "rows": rows, "meta": meta, "full": full})

    # ── What the window asks (passed on by Rust) ──────────────────────────────

    @Slot(object)
    def _run_gui_call(self, fn):
        fn()

    def _window_call(self, args: List[Any]) -> Any:
        method, call_args = args[0], list(args[1] if len(args) > 1 else [])
        if method != "toggleFavorite":
            raise AttributeError(f"watchlistModel has no method {method!r} here")
        done = threading.Event()
        box: List[Any] = [None, None]

        def run():
            try:
                box[0] = self.toggleFavorite(*call_args)
            except Exception as e:
                box[1] = e
            finally:
                done.set()
        self._guiCall.emit(run)
        done.wait(30)
        if box[1] is not None:
            raise box[1]
        self.refresh()
        return box[0]
