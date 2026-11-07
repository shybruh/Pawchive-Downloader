"""
The window's queue view (file list, counts, per-post groups) answered by the desktop shell's Rust
port (desktop/src-tauri/src/downloader/queue_view.rs, served by src-tauri/src/host/queue.rs).

RustQueueModel is the app's QueueModel with its files mirrored to Rust: the bridge still keeps
its task list here and changes it as before, and every change (new files, removed ones, status
and progress) goes over to Rust a moment later. The window reads "appBridge.queueModel" and its
groupsModel from Rust; what the window asks of them (pause a post, retry its failed files…) is
passed back here and runs on this model, so the bridge's handlers don't change.

Without the desktop shell, the Python model is what the window reads (as before).
"""

import threading
import time
from typing import Any, Dict, List, Optional

from bridge.queue_model import QueueModel
from core.logger import logger
from services import rust_queue


def available() -> bool:
    return rust_queue.available()


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
    def __init__(self, parent=None, enable_groups: bool = True):
        super().__init__(parent, enable_groups)
        self._sent: Dict[str, Dict[str, Any]] = {}       # key -> the row Rust has
        self._sent_order: Optional[List[str]] = None
        self._sent_meta: Optional[Dict[str, Any]] = None
        self._sent_on = None                              # the connection they went over
        self._full = True
        self._dirty: Dict[int, Any] = {}
        self._lock = threading.Lock()
        self._wake = threading.Event()
        for sig in (self.modelReset, self.rowsInserted, self.rowsRemoved, self.groupsChanged, self.cleared,
                    self.countsChanged, self.failedCountChanged):
            sig.connect(self._changed_all)
        changed_404 = getattr(parent, "skipRetry404Changed", None)
        if changed_404 is not None:
            changed_404.connect(self._changed_all)
        link = rust_queue.shared_link()
        link.handlers["queue_call"] = self._window_call
        link.on_connect.append(self._changed_all)
        threading.Thread(target=self._flush_loop, name="rust-queue-view", daemon=True).start()
        self._wake.set()

    # ── Changes go to Rust ────────────────────────────────────────────────────

    def updateTask(self, task):
        super().updateTask(task)
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
        saved = [0, 0]
        if self._failed_count == 0:
            rm = getattr(p, "recovery_manager", None) if p else None
            if rm is not None and hasattr(rm, "retry_counts"):
                try:
                    counts = rm.retry_counts(wait=False) or rm.retry_counts_cached()
                    saved = [int(counts[0]), int(counts[1])]
                except Exception:
                    pass
        paused = self._groups_model._paused_batch_ids if self._groups_model else self._paused_batch_ids
        return {"paused": sorted(paused), "skip_404": bool(getattr(p, "skipRetry404", False)) if p else False,
                "saved_failed": saved}

    def _push(self):
        link = rust_queue.shared_link()
        if not link.connect():
            return
        if self._sent_on is not link._sock:         # (a new connection: Rust has nothing yet)
            self._sent_on = link._sock
            self._sent, self._sent_meta, self._sent_order = {}, None, None
            self._full = True
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
        meta = self._meta()
        if not rows and order is None and meta == self._sent_meta:
            return
        self._sent_meta = meta
        link.send({"op": "view", "order": order, "rows": rows, "meta": meta})

    # ── What the window asks (passed on by Rust) ──────────────────────────────

    def retryTaskById(self, file_id: str):
        """retryTaskAt for the window's rows (they are numbered in Rust)."""
        t = next((t for t in self._tasks if str(t.file_id) == str(file_id)), None)
        if t is None:
            return
        t.retry_count = getattr(t, "retry_count", 0) + 1
        t.retry_capped = False
        t.status = "pending"
        t.error_msg = ""
        t.progress_pct = 0
        self.updateTask(t)
        self.singleRetryRequested.emit(t.file_id)

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
