"""
The window's queue view (file list, counts, per-post groups) answered by the desktop shell's Rust
port (desktop/src-tauri/src/downloader/queue_view.rs, served by src-tauri/src/host/queue.rs).

The view's rows are the engine's own queue (queue_view.rs follow); what goes over from here is what
the view needs from the app: the paused posts, "don't retry 404s" and the failed files saved for
Retry Failed. With the old Python downloader (PAWCHIVE_PYTHON_DOWNLOADER=1) the engine's queue isn't
used, so the bridge's list (bridge/queue_model.py) is sent over too, file by file as it changes.

Pausing, resuming, cancelling, removing and retrying a post or a file are done by Rust's queue (the
bridge follows: AppBridge._rust_queue_action); the rest of what the window asks (the Retry list
saved on disk…) is passed back here and runs on this list, so the bridge's handlers don't change.
"""

import threading
import time
from typing import Any, Dict, List, Optional

from bridge.queue_model import QueueModel
from core.logger import logger
from services import rust_queue


def available() -> bool:
    """The desktop shell is there to show the view."""
    return bool(rust_queue._URL and rust_queue._TOKEN)


def row_of(t) -> Dict[str, Any]:
    """A file as Rust's view takes it (queue_view.rs Row)."""
    return {
        "key": rust_queue._key(t), "url": t.url or "", "target_path": t.target_path or "",
        "post_title": t.post_title or "", "creator_name": t.creator_name or "", "service": t.service or "",
        "post_id": str(t.post_id or ""), "file_id": str(t.file_id or ""), "batch_id": getattr(t, "batch_id", "") or "",
        "status": t.status or "", "file_size": int(t.file_size or 0), "downloaded_bytes": int(t.downloaded_bytes or 0),
        "error_msg": t.error_msg or "", "speed_str": t.speed_str or "", "eta_str": t.eta_str or "",
        "progress_pct": t.progress_pct, "retry_count": int(getattr(t, "retry_count", 0) or 0),
        "http_status": int(getattr(t, "http_status", 0) or 0),
    }


class RustQueueModel(QueueModel):
    def __init__(self, parent=None):
        super().__init__(parent)
        # The old Python downloader: Rust's view takes the files from here
        self._send_rows = not rust_queue.available()
        self._sent: Dict[str, Dict[str, Any]] = {}       # key -> the row Rust has
        self._sent_order: Optional[List[str]] = None
        self._sent_meta: Optional[Dict[str, Any]] = None
        self._sent_on = None                              # the connection they went over
        self._full = True
        self._dirty: Dict[int, Any] = {}
        self._lock = threading.Lock()
        self._wake = threading.Event()
        self.changed.connect(self._changed_all)
        self.failedCountChanged.connect(self._changed_all)
        changed_404 = getattr(parent, "skipRetry404Changed", None)
        if changed_404 is not None:
            changed_404.connect(self._changed_all)
        link = rust_queue.shared_link()
        link.handlers["queue_call"] = self._window_call
        link.on_connect.append(self._changed_all)
        threading.Thread(target=self._flush_loop, name="rust-queue-view", daemon=True).start()
        self._wake.set()

    @property
    def groups(self) -> List[Dict[str, Any]]:
        link = rust_queue.shared_link()
        ok, value = link.call("view_groups", {}) if link.connect() else (False, "not connected")
        if not ok:
            logger.debug(f"Couldn't read the queue's groups: {value}", category="queue")
            return []
        return list(value or [])

    # ── Changes go to Rust ────────────────────────────────────────────────────

    def updateTask(self, task):
        super().updateTask(task)
        if self._send_rows:
            with self._lock:
                self._dirty[id(task)] = task
            self._wake.set()

    def _changed_all(self, *args):
        self._full = True
        self._wake.set()

    def _flush_loop(self):
        while True:
            self._wake.wait()
            time.sleep(0.1)
            self._wake.clear()
            try:
                self._push()
            except Exception as e:
                logger.debug(f"Couldn't update the queue view: {e}", category="queue")

    def _meta(self) -> Dict[str, Any]:
        p = self.parent()
        saved = [0, 0]           # (Rust shows them only when its queue has no failed files)
        rm = getattr(p, "recovery_manager", None) if p else None
        if rm is not None and hasattr(rm, "retry_counts"):
            try:
                counts = rm.retry_counts(wait=False) or rm.retry_counts_cached()
                saved = [int(counts[0]), int(counts[1])]
            except Exception:
                pass
        return {"paused": sorted(self._paused_batch_ids), "skip_404": self._skip_404(), "saved_failed": saved}

    def _rows(self, new_link: bool):
        """The old Python downloader: the files Rust's view doesn't have yet, and the order when it changed."""
        if new_link:
            self._sent, self._sent_order, self._full = {}, None, True
        with self._lock:
            full, self._full = self._full, False
            dirty, self._dirty = self._dirty, {}
        tasks = list(self._tasks) if full else list(dirty.values())
        rows = []
        for t in tasks:
            r = row_of(t)
            if self._sent.get(r["key"]) != r:
                self._sent[r["key"]] = r
                rows.append(r)
        order = None
        if full:
            order = [rust_queue._key(t) for t in tasks]
            keep = set(order)
            for k in [k for k in self._sent if k not in keep]:
                del self._sent[k]
            if order == self._sent_order:
                order = None                        # (same files in the same order)
            else:
                self._sent_order = order
        return order, rows

    def _push(self):
        link = rust_queue.shared_link()
        if not link.connect():
            return
        new_link = self._sent_on is not link._sock  # (a new connection: Rust has nothing yet)
        if new_link:
            self._sent_on = link._sock
            self._sent_meta = None
        order, rows = self._rows(new_link) if self._send_rows else (None, [])
        meta = self._meta()
        if not rows and order is None and meta == self._sent_meta:
            return
        self._sent_meta = meta
        link.send({"op": "view", "order": order, "rows": rows, "meta": meta})

    # ── What the window asks (passed on by Rust) ──────────────────────────────

    _WINDOW_METHODS = {
        "removeBatch", "cancel_all_pending", "cancelBatch", "pauseBatch", "resumeBatch", "pauseTask", "resumeTask",
        "isBatchPaused", "toggleBatchPause", "retryBatch", "clear", "clearFailedTasks", "retryFailed",
        "retryTaskById", "getFailedTasksList", "prepareFailedTasksAsync", "getFailedTasksPage", "retrySelected",
    }

    def _window_call(self, args: List[Any]) -> Any:
        """Runs one of the window's requests on the window thread and returns its answer."""
        method, call_args = args[0], list(args[1] if len(args) > 1 else [])
        if method not in self._WINDOW_METHODS:
            raise AttributeError(f"queueModel has no method {method!r}")
        done = threading.Event()
        box: List[Any] = [None, None]

        def run():
            try:
                box[0] = getattr(self, method)(*call_args)
            except Exception as e:
                box[1] = e
            finally:
                done.set()
        self._guiCall.emit(run)
        done.wait(30)
        if box[1] is not None:
            raise box[1]
        return box[0]
