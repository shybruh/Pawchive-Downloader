"""
The bridge's copy of the download queue: the files in order, with what the bridge's export,
history, retry and session code reads from it, and the window's queue requests that are still
answered here (services/rust_queue_view.py passes them on).

The window doesn't read this list: its file list, filters, counts and per-post groups are the Rust
queue view (desktop/src-tauri/src/downloader/queue_view.rs), served by the desktop shell's host.
"""

import threading
from typing import Any, Dict, List, Optional

from PySide6.QtCore import QObject, Qt, QThread, Signal, Slot

from core.downloader import DownloadTask
from core.logger import logger

_BUSY = ("pending", "downloading", "retrying")


def format_size(b: int) -> str:
    if b <= 0:
        return "-"
    if b >= 1024 * 1024 * 1024:
        return f"{b / (1024 * 1024 * 1024):.2f} GB"
    if b >= 1024 * 1024:
        return f"{b / (1024 * 1024):.1f} MB"
    if b >= 1024:
        return f"{b / 1024:.1f} KB"
    return f"{b} B"


def batch_of(t) -> str:
    return getattr(t, "batch_id", "") or f"{t.service}_{t.creator_name}_{t.post_id}".strip("_")


def _is_404(t) -> bool:
    return "404" in str(getattr(t, "error_msg", "") or "").lower() or getattr(t, "http_status", 0) == 404


def _stopped(t, status: str) -> None:
    t.status = status
    t.speed_bps = 0
    t.speed_str = "0 KB/s"
    t.eta_str = "--"


def _cancelled(t) -> None:
    _stopped(t, "cancelled")
    t.error_msg = "Download cancelled by user"
    t.progress_pct = 0


def _for_retry(t) -> None:
    t.retry_count = getattr(t, "retry_count", 0) + 1
    t.retry_capped = False
    t.status = "pending"
    t.error_msg = ""
    t.progress_pct = 0


def _was_retried(t) -> bool:
    return t.status == "pending" and (getattr(t, "retry_count", 0) > 0 or bool(getattr(t, "error_msg", "")))


class QueueModel(QObject):
    # Something about the list changed (files added / removed, a status, a paused post)
    changed = Signal()
    failedCountChanged = Signal()
    failedListReady = Signal(str, int)
    retryRequested = Signal()
    singleRetryRequested = Signal(str)
    retrySelectedRequested = Signal(list)
    batchRetryRequested = Signal(str)
    batchCancelRequested = Signal(str)
    batchRemoveRequested = Signal(str)
    batchPauseRequested = Signal(str)
    batchResumeRequested = Signal(str)
    taskPauseRequested = Signal(str)     # file_id
    taskResumeRequested = Signal(str)    # file_id
    cleared = Signal()
    _guiCall = Signal(object)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._guiCall.connect(self._run_gui_call, Qt.QueuedConnection)
        self._tasks: List[DownloadTask] = []
        self._paused_batch_ids: set = set()
        self._task_last_status: Dict[int, str] = {}
        # (url, path) and file ids already in the queue, kept between appends (rebuilding them from
        # the whole queue on every append made adding to a big queue slow)
        self._signatures: set = set()
        self._signatures_dirty = False
        self._failed_rows: List[Dict[str, Any]] = []
        self._active_failed_request: Optional[str] = None

    @staticmethod
    def _format_size(b: int) -> str:
        return format_size(b)

    def _skip_404(self) -> bool:
        p = self.parent()
        return bool(getattr(p, "skipRetry404", False)) if p else False

    def _changed(self, failed: bool = False) -> None:
        self.changed.emit()
        if failed:
            self.failedCountChanged.emit()

    @Slot(object)
    def _run_gui_call(self, fn):
        fn()

    def _off_gui_thread(self, fn) -> bool:
        """The list changes on the window thread. Called from another thread, fn is queued for it and
        True is returned."""
        if QThread.currentThread() is not self.thread():
            self._guiCall.emit(fn)
            return True
        return False

    # ── The list ──────────────────────────────────────────────────────────────

    def getTasks(self) -> List[DownloadTask]:
        return list(self._tasks)

    @property
    def tasks(self) -> List[DownloadTask]:
        return list(self._tasks)

    def size(self) -> int:
        return len(self._tasks)

    @property
    def pendingCount(self) -> int:
        return sum(1 for t in self._tasks if t.status in ("pending", "paused"))

    @property
    def groups(self) -> List[Dict[str, Any]]:
        """The per-post groups as the window shows them (saved with the session and the export)."""
        return []

    def setTasks(self, tasks: List[DownloadTask]):
        tasks = list(tasks)
        if self._off_gui_thread(lambda: self.setTasks(tasks)):
            return
        self._tasks = tasks
        self._task_last_status = {id(t): t.status for t in tasks}
        self._signatures_dirty = True
        self._changed(True)

    def _ensure_signatures(self):
        if self._signatures_dirty:
            sigs = set()
            for t in self._tasks:
                sigs.add((t.url, t.target_path))
                if t.file_id:
                    sigs.add(t.file_id)
            self._signatures = sigs
            self._signatures_dirty = False

    def appendTasks(self, tasks: List[DownloadTask]) -> int:
        if not tasks:
            return 0
        tasks = list(tasks)
        if self._off_gui_thread(lambda: self.appendTasks(tasks)):
            return len(tasks)
        self._ensure_signatures()
        sigs = self._signatures
        added = []
        for t in tasks:
            sig1, sig2 = (t.url, t.target_path), t.file_id
            if sig1 in sigs or (sig2 and sig2 in sigs):
                continue
            sigs.add(sig1)
            if sig2:
                sigs.add(sig2)
            self._task_last_status[id(t)] = t.status
            added.append(t)
        if added:
            self._tasks.extend(added)
            self._changed(True)
        return len(added)

    def addTasks(self, tasks: List[DownloadTask]):
        self.appendTasks(tasks)

    def add_tasks(self, tasks: List[DownloadTask]):
        return self.appendTasks(tasks)

    def updateTask(self, task: DownloadTask):
        """A file changed (progress or status)."""
        old = self._task_last_status.get(id(task))
        if old != task.status:
            self._task_last_status[id(task)] = task.status
            self._changed(old in ("failed", "cancelled") or task.status in ("failed", "cancelled"))

    def clear(self):
        if self._off_gui_thread(self.clear):
            return
        self._paused_batch_ids.clear()
        self._tasks = []
        self._task_last_status.clear()
        self._signatures_dirty = True
        self._changed(True)
        self.cleared.emit()

    def _set_status(self, tasks) -> None:
        for t in tasks:
            self._task_last_status[id(t)] = t.status

    # ── A post ────────────────────────────────────────────────────────────────

    def isBatchPaused(self, batch_id: str) -> bool:
        return bool(batch_id) and batch_id in self._paused_batch_ids

    def toggleBatchPause(self, batch_id: str):
        if self.isBatchPaused(batch_id):
            self.resumeBatch(batch_id)
        else:
            self.pauseBatch(batch_id)

    def removeBatch(self, batch_id: str):
        if not batch_id:
            return
        self._paused_batch_ids.discard(batch_id)
        self._tasks = [t for t in self._tasks if batch_of(t) != batch_id or t.status == "downloading"]
        self._signatures_dirty = True
        self._changed(True)
        self.batchRemoveRequested.emit(batch_id)

    def cancelBatch(self, batch_id: str):
        if not batch_id:
            return
        self._paused_batch_ids.discard(batch_id)
        hit = [t for t in self._tasks if batch_of(t) == batch_id and t.status in _BUSY + ("paused",)]
        for t in hit:
            _cancelled(t)
        self._set_status(hit)
        self._changed(True)
        self.batchCancelRequested.emit(batch_id)

    def pauseBatch(self, batch_id: str):
        if not batch_id:
            return
        self._paused_batch_ids.add(batch_id)
        hit = [t for t in self._tasks if batch_of(t) == batch_id and t.status in _BUSY]
        for t in hit:
            _stopped(t, "paused")
        self._set_status(hit)
        self._changed()
        self.batchPauseRequested.emit(batch_id)

    def resumeBatch(self, batch_id: str):
        if not batch_id:
            return
        self._paused_batch_ids.discard(batch_id)
        hit = [t for t in self._tasks if batch_of(t) == batch_id and t.status == "paused"]
        for t in hit:
            t.status = "pending"
            t.error_msg = ""
        self._set_status(hit)
        self._changed()
        self.batchResumeRequested.emit(batch_id)

    def retryBatch(self, batch_id: str):
        if not batch_id:
            return
        self._paused_batch_ids.discard(batch_id)
        skip_404 = self._skip_404()
        hit = [t for t in self._tasks if batch_of(t) == batch_id and t.status in ("failed", "cancelled", "pending")
               and not (skip_404 and _is_404(t))]
        for t in hit:
            t.status = "pending"
            t.error_msg = ""
            t.retry_count = getattr(t, "retry_count", 0) + 1
        self._set_status(hit)
        self._changed(True)
        self.batchRetryRequested.emit(batch_id)

    def cancel_all_pending(self):
        """Cancels every waiting and running file at once."""
        hit = [t for t in self._tasks if t.status in _BUSY]
        for t in hit:
            _cancelled(t)
        self._set_status(hit)
        self._changed(True)

    # ── A file ────────────────────────────────────────────────────────────────

    def _set_task_status(self, file_id: str, from_states: tuple, to_state: str) -> bool:
        t = next((t for t in self._tasks if str(getattr(t, "file_id", "")) == str(file_id)), None)
        if t is None or t.status not in from_states:
            return False
        if to_state == "paused":
            _stopped(t, "paused")
        else:
            t.status = to_state
            t.error_msg = ""
        self._set_status([t])
        self._changed()
        return True

    def pauseTask(self, file_id: str):
        """Pauses a single file; a file mid-download stops and keeps what it has so far."""
        if self._set_task_status(file_id, _BUSY, "paused"):
            self.taskPauseRequested.emit(file_id)

    def resumeTask(self, file_id: str):
        if self._set_task_status(file_id, ("paused",), "pending"):
            self.taskResumeRequested.emit(file_id)

    def retryTaskById(self, file_id: str):
        t = next((t for t in self._tasks if str(t.file_id) == str(file_id)), None)
        if t is None:
            return
        _for_retry(t)
        self.updateTask(t)
        self.singleRetryRequested.emit(t.file_id)

    # ── Failed files ──────────────────────────────────────────────────────────

    def clearFailedTasks(self, selected_file_ids: Optional[List[str]] = None):
        """Removes failed and cancelled files (all, or the ones picked)."""
        picked = set(selected_file_ids) if selected_file_ids else None
        self._tasks = [t for t in self._tasks if not (t.status in ("failed", "cancelled")
                                                       and (picked is None or (t.file_id or t.url or t.filename) in picked))]
        self._signatures_dirty = True
        self._changed(True)

    def retryFailed(self):
        """Every failed and cancelled file back to waiting; retryRequested follows."""
        skip_404 = self._skip_404()
        failed = [t for t in self._tasks if (t.status in ("failed", "cancelled") or _was_retried(t))
                  and not (skip_404 and _is_404(t))]
        if not failed:
            return
        for t in failed:
            _for_retry(t)
        self._set_status(failed)
        self._changed(True)
        self.retryRequested.emit()

    def retrySelected(self, selected_file_ids: List[str]):
        """Only the picked failed / cancelled files back to waiting."""
        skip_404 = self._skip_404()
        picked = set(selected_file_ids)
        for t in self._tasks:
            if t.status in ("failed", "cancelled", "pending") and (t.file_id or t.url or t.filename) in picked:
                if skip_404 and _is_404(t):
                    continue
                _for_retry(t)
                self.updateTask(t)
        self.retrySelectedRequested.emit(selected_file_ids)

    def getFailedTasksList(self):
        """The Retry dialog's rows."""
        return self._build_failed_list(list(self._tasks), self._skip_404(), self.parent())

    def prepareFailedTasksAsync(self, request_id: str):
        """Builds the Retry dialog's list in the background (it stalled the window with big queues and
        failed lists); failedListReady(request_id, count) follows, then the dialog takes it a page at a
        time with getFailedTasksPage."""
        p, skip_404, tasks = self.parent(), self._skip_404(), list(self._tasks)
        self._active_failed_request = request_id

        def _job():
            try:
                rows = self._build_failed_list(tasks, skip_404, p)
            except Exception as e:
                logger.debug(f"Couldn't list the failed files: {e}", category="queue")
                rows = []
            if self._active_failed_request != request_id:
                return
            self._failed_rows = rows
            try:
                self.failedListReady.emit(request_id, len(rows))
            except RuntimeError:
                pass
        threading.Thread(target=_job, name="FailedList", daemon=True).start()

    def getFailedTasksPage(self, offset: int, limit: int):
        rows = self._failed_rows or []
        return rows[max(0, offset):max(0, offset) + max(0, limit)]

    @staticmethod
    def _failed_row(d: Dict[str, Any], err_msg: str) -> Dict[str, Any]:
        p_url = d.get("post_url") or ""
        if not p_url and d.get("service") and d.get("post_id"):
            p_url = f"https://pawchive.pw/{d.get('service')}/user/{d.get('creator_name')}/post/{d.get('post_id')}"
        retries = d.get("retry_count") or 0
        return {
            "fileId": d.get("file_id", ""), "filename": d.get("filename", ""), "postTitle": d.get("post_title", ""),
            "creatorName": d.get("creator_name", ""), "service": d.get("service", ""), "postId": d.get("post_id", ""),
            "postUrl": p_url, "url": d.get("url", ""), "errorMsg": err_msg,
            "fileSize": format_size(d.get("file_size") or 0), "retryCount": retries,
            "retryCapped": bool(d.get("retry_capped")) or retries >= 5,
        }

    def _build_failed_list(self, tasks, skip_404: bool, p):
        """The Retry dialog's rows: failed / cancelled files of the queue, else the saved failed files."""
        failed = []
        in_queue = False
        for t in tasks:
            if not (t.status in ("failed", "cancelled") or _was_retried(t)):
                continue
            in_queue = True
            err_msg = t.error_msg or ("Download cancelled by user" if t.status == "cancelled" else "Download failed")
            if skip_404 and ("404" in str(err_msg).lower() or getattr(t, "http_status", 0) == 404):
                continue
            d = {k: getattr(t, k, "") for k in ("file_id", "filename", "post_title", "creator_name", "service",
                                                 "post_id", "post_url", "url", "file_size", "retry_count", "retry_capped")}
            failed.append(self._failed_row(d, err_msg))
        if in_queue:
            return failed
        try:
            for item in (p.recovery_manager.load_retries() if p and hasattr(p, "recovery_manager") else None) or []:
                d = item.to_dict() if hasattr(item, "to_dict") else (item if isinstance(item, dict) else vars(item))
                err_msg = d.get("error_msg") or "Download failed"
                if skip_404 and ("404" in str(err_msg).lower() or d.get("http_status") == 404):
                    continue
                failed.append(self._failed_row(d, err_msg))
        except Exception as e:
            logger.debug(f"Error loading retries from recovery_manager in getFailedTasksList: {e}")
        return failed
