"""
Downloads the queue with the desktop shell's Rust download engine (desktop/src-tauri/src/downloader/).

RustDownloader is the app's FileDownloader with its run loop replaced: the queue's files, their
DownloadTask objects and the window's models stay here, while picking files, downloading them
(servers, retries, the 429 cooldown, the adaptive thread count, pausing) and the speed / ETA run
in Rust. This module keeps the two in step over a local socket:

  * before a run, and whenever the task list is replaced while one runs, the queue is sent over;
  * status changes made here (pause a file, cancel a batch, retry) are passed on;
  * every change Rust makes to a file comes back and is applied to its DownloadTask, with the
    same callbacks the Python loop fired, so the window, history and watchlist don't notice.

Rust asks back for what still lives here: the download archive, login cookies, refreshed
Pawchive links, embedded videos (yt-dlp) and Telegram files, and post-processing.

Without the desktop shell (no PAWCHIVE_QUEUE_URL), the Python loop runs as before.
"""

import itertools
import json
import os
import socket
import threading
import time
from typing import Any, Dict, List, Optional

import core.downloader as dl
from core.downloader import FileDownloader
from core.logger import logger

_URL = os.environ.get("PAWCHIVE_QUEUE_URL", "")
_TOKEN = os.environ.get("PAWCHIVE_QUEUE_TOKEN", "")

_applying = threading.local()          # set while a change from Rust is applied (not sent back)
_keys = itertools.count(1)
# Rust's log lines for controls this side logs itself
_ALREADY_LOGGED = {"Download paused.", "Download resumed.", "Download cancellation requested."}


def available() -> bool:
    return bool(_URL and _TOKEN) and os.environ.get("PAWCHIVE_PYTHON_DOWNLOADER", "") != "1"


def _key(t) -> str:
    d = t.__dict__
    k = d.get("_rkey")
    if not k:
        k = d["_rkey"] = f"t{next(_keys)}"
    return k


class _Link:
    """The socket to the Rust queue: one JSON object per line, both ways."""

    def __init__(self, on_message):
        self._on_message = on_message
        self._sock: Optional[socket.socket] = None
        self._lock = threading.Lock()
        self._connect_lock = threading.Lock()

    @property
    def up(self) -> bool:
        return self._sock is not None

    def connect(self) -> bool:
        with self._connect_lock:
            if self._sock is not None:
                return True
            try:
                host, port = _URL[len("tcp://"):].rsplit(":", 1)
                s = socket.create_connection((host, int(port)), timeout=5)
                s.settimeout(None)
                s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                s.sendall((json.dumps({"token": _TOKEN}) + "\n").encode())
            except OSError as e:
                logger.warning(f"The download engine couldn't be reached ({e}); downloading the old way.", category="downloader")
                return False
            self._sock = s
            threading.Thread(target=self._read, args=(s,), name="rust-queue-read", daemon=True).start()
            return True

    def send(self, obj: Dict[str, Any]) -> bool:
        s = self._sock
        if s is None:
            return False
        data = (json.dumps(obj, ensure_ascii=False, default=str) + "\n").encode("utf-8")
        try:
            with self._lock:
                s.sendall(data)
            return True
        except OSError:
            self._close(s)
            return False

    def _close(self, s):
        if self._sock is s:
            self._sock = None
        try:
            s.close()
        except OSError:
            pass

    def _read(self, s):
        f = s.makefile("r", encoding="utf-8", newline="\n")
        try:
            for line in f:
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue
                try:
                    self._on_message(msg)
                except Exception as e:
                    logger.debug(f"Download engine message failed: {e}", category="downloader")
        except OSError:
            pass
        self._close(s)
        try:
            self._on_message({"t": "gone"})
        except Exception:
            pass


class RustDownloader(FileDownloader):
    def __init__(self, *args, **kwargs):
        self._link = None
        self._mw = 4
        super().__init__(*args, **kwargs)
        self._link = _Link(self._on_message)
        self._by_key: Dict[str, Any] = {}
        self._finished = threading.Event()
        self._result = (False, "")
        self._dirty: Dict[str, Any] = {}
        self._dirty_lock = threading.Lock()
        self._dirty_wake = threading.Event()
        dl.status_listener = self._status_changed
        threading.Thread(target=self._flush_loop, name="rust-queue-flush", daemon=True).start()

    # ── Worker count (adaptive threading changes it on the Rust side) ──────────

    @property
    def max_workers(self) -> int:
        return self._mw

    @max_workers.setter
    def max_workers(self, n: int) -> None:
        self._mw = max(1, int(n or 1))
        if self._link is not None and self._link.up:
            self._link.send({"op": "workers", "n": self._mw})

    # ── Keeping Rust's copy of the queue up to date ─────────────────────────────

    def _payload(self, t, options_keys: Dict[int, str], options_table: Dict[str, Any]) -> Dict[str, Any]:
        o = getattr(t, "options", None)
        opt = ""
        if o is not None and hasattr(o, "to_dict"):
            opt = options_keys.get(id(o))
            if opt is None:
                opt = options_keys[id(o)] = f"o{len(options_keys) + 1}"
                options_table[opt] = o.to_dict()
        key = _key(t)
        return {
            "key": key, "url": t.url or "", "target_path": t.target_path or "", "post_title": t.post_title or "",
            "creator_name": t.creator_name or "", "service": t.service or "", "post_id": str(t.post_id or ""),
            "file_id": str(t.file_id or ""), "file_size": int(t.file_size or 0),
            "expected_sha256": str(getattr(t, "expected_sha256", "") or ""), "is_ytdlp": bool(t.is_ytdlp),
            "is_telegram": bool(getattr(t, "is_telegram", False)), "batch_id": getattr(t, "batch_id", "") or "",
            "post_url": getattr(t, "post_url", "") or "", "post_date": getattr(t, "post_date", "") or "",
            "user_id": str(getattr(t, "user_id", "") or ""),
            "fallback_urls": [u for u in (getattr(t, "fallback_urls", None) or []) if isinstance(u, str)],
            "post_info_path": getattr(t, "post_info_path", "") or "",
            "status": t.status, "error_msg": t.error_msg or "", "retry_count": int(getattr(t, "retry_count", 0) or 0),
            "downloaded_bytes": int(t.downloaded_bytes or 0), "http_status": int(getattr(t, "http_status", 0) or 0),
            "opt": opt,
        }

    def _sync(self, replace: bool) -> None:
        tasks = list(self.tasks)
        keys: Dict[int, str] = {}
        table: Dict[str, Any] = {}
        payload = [self._payload(t, keys, table) for t in tasks]
        self._by_key = {p["key"]: t for p, t in zip(payload, tasks)}
        self._link.send({"op": "sync", "tasks": payload, "options": table, "replace": replace})

    def _status_changed(self, task) -> None:
        if getattr(_applying, "on", False) or self._link is None or not self._link.up:
            return
        k = task.__dict__.get("_rkey")
        if not k:
            return
        with self._dirty_lock:
            self._dirty[k] = task
        self._dirty_wake.set()

    def _flush_loop(self) -> None:
        """Status changes made here go to Rust a moment later, with the error text and retry count
        set right after the status (the retry functions change them in that order)."""
        while True:
            self._dirty_wake.wait()
            time.sleep(0.05)
            self._dirty_wake.clear()
            with self._dirty_lock:
                dirty, self._dirty = self._dirty, {}
            for k, t in dirty.items():
                self._link.send({"op": "state", "key": k, "status": t.status, "error_msg": t.error_msg or "",
                                 "retry_count": int(getattr(t, "retry_count", 0) or 0)})

    # ── The run ────────────────────────────────────────────────────────────────

    def _run_download_loop(self, options, cookie_str: str, session_id: int = 0):
        if not self._link.connect():
            return super()._run_download_loop(options, cookie_str, session_id)
        self._keep_paths_apart(self.tasks, [])
        self._save_recovery_checkpoint(options, async_write=True)
        self.current_options = options
        self.start_time = time.time()
        self.downloaded_bytes = 0
        clean_cookie = (cookie_str or "").strip()
        if not clean_cookie:
            try:
                from core.auth_manager import auth_manager, GENERAL_COOKIE_SLOT
                clean_cookie = auth_manager.get_credential(GENERAL_COOKIE_SLOT, "cookie")
            except Exception:
                pass
        if clean_cookie and ((clean_cookie.startswith("eyJ") and "session=" not in clean_cookie) or "=" not in clean_cookie):
            clean_cookie = f"session={clean_cookie}"
        self._clean_cookie = clean_cookie
        logger.debug("Download settings saved to the log file.", category="downloader",
                     details=self._describe_run(options, cookie_str, sum(1 for t in self.tasks if t.status == "pending")))

        self._finished.clear()
        self._sync(replace=True)
        archive = bool(self.archive_manager and self.archive_manager.is_enabled)
        self._link.send({"op": "start", "options": options.to_dict(), "archive": archive, "workers": self._mw})
        sig = (id(self.tasks), len(self.tasks))
        while not self._finished.wait(0.5):
            if session_id != self._session_id:
                return          # superseded (cancelled, or a newer run started)
            if not self._link.up:
                self._result = (False, "The download engine stopped")
                break
            now_sig = (id(self.tasks), len(self.tasks))
            if now_sig != sig:  # files were added or the list was replaced while running
                sig = now_sig
                self._sync(replace=False)
        if session_id != self._session_id:
            return
        self._close_worker_sessions()
        if self.on_cleanup_wanted:
            try:
                self.on_cleanup_wanted()
            except Exception:
                pass
        self._is_running = False
        for t in self.tasks:
            if t.status == "downloading":           # (only when the engine went away)
                t.status = "pending"
        live = {(t.post_info_path, getattr(t, "post_id", "")) for t in self.tasks
                if getattr(t, "post_info_path", "") and t.status not in ("completed", "skipped")}
        live_paths = {p for p, _ in live}
        for k in list(self._post_info_pending):
            if (k not in live) if isinstance(k, tuple) else (k not in live_paths):
                self._post_info_pending.pop(k, None)
        ok, summary = self._result
        if self.on_download_finished:
            self.on_download_finished(ok, summary if ok else (summary or "Download cancelled by user."))

    # ── Messages from Rust ──────────────────────────────────────────────────────

    def _on_message(self, msg: Dict[str, Any]) -> None:
        t = msg.get("t")
        if t == "task":
            self._apply(msg)
        elif t == "progress":
            info = msg.get("info") or {}
            self.downloaded_bytes = int(info.get("downloaded_bytes") or 0)
            self.adaptive_state = info.get("adaptive_state", self.adaptive_state)
            self.adaptive_status_text = info.get("adaptive_status_text", self.adaptive_status_text)
            if self.on_progress_update:
                self.on_progress_update(info)
        elif t == "finished":
            self._result = (bool(msg.get("ok")), str(msg.get("summary") or ""))
            self._finished.set()
        elif t == "workers":
            self._mw = int(msg.get("n") or 1)
            if self.on_concurrency_throttled:
                self.on_concurrency_throttled(self._mw)
        elif t == "log":
            if msg.get("message") in _ALREADY_LOGGED:
                return
            level = msg.get("level", "info")
            getattr(logger, level if level in ("debug", "info", "warning", "error", "success") else "info")(
                str(msg.get("message", "")), category="downloader")
        elif t == "checkpoint":
            self._save_recovery_checkpoint(async_write=True)
        elif t == "discard_recovery":
            rec = getattr(self.session_manager, "recovery_manager", None)
            if rec:
                rec.discard_recovery()
        elif t == "record_file":
            task = self._by_key.get(msg.get("key"))
            if task is not None and self.archive_manager and self.archive_manager.is_enabled:
                self.archive_manager.record_file(
                    service=task.service, creator_id=task.user_id, post_id=task.post_id, file_id=task.file_id,
                    file_hash=task.expected_sha256, filename=task.filename, creator_name=task.creator_name,
                    post_title=task.post_title, file_size=int(msg.get("size") or 0), file_path=msg.get("path") or task.target_path)
        elif t == "ask":
            threading.Thread(target=self._answer, args=(msg,), daemon=True).start()
        elif t == "gone":
            self._finished.set()

    def _apply(self, m: Dict[str, Any]) -> None:
        task = self._by_key.get(m.get("key"))
        if task is None:
            return
        old = task.status
        _applying.on = True
        try:
            for name in ("url", "target_path", "error_msg", "speed_str", "eta_str"):
                if name in m:
                    setattr(task, name, m[name] or ("" if name not in ("speed_str", "eta_str") else getattr(task, name)))
            for name in ("downloaded_bytes", "file_size", "progress_pct", "speed_bps", "retry_count", "http_status"):
                if name in m:
                    setattr(task, name, int(m[name] or 0))
            if "used_preview" in m:
                task.used_preview = bool(m["used_preview"])
            if "retry_capped" in m:
                task.retry_capped = bool(m["retry_capped"])
            if "fallback_urls" in m:
                task.fallback_urls = list(m["fallback_urls"] or [])
            if m.get("tried_urls"):
                task._tried_urls = list(m["tried_urls"])
            new = m.get("status") or old
            task.status = new
        finally:
            _applying.on = False
        if new != old:
            if new == "completed":
                try:
                    self.session_manager.record_downloaded_file(task.file_id)
                except Exception:
                    pass
                self._write_pending_post_info(task)
            elif new == "skipped":
                self._write_pending_post_info(task)
        if self.on_task_status_changed:
            self.on_task_status_changed(task)

    def _answer(self, msg: Dict[str, Any]) -> None:
        fn, args = msg.get("fn"), msg.get("args") or []
        value: Any = None
        try:
            task = self._by_key.get(args[0]) if args and fn in ("is_archived", "refresh_link", "special", "post_process") else None
            if fn == "cookie":
                from core.providers import cookie_for_url
                value = cookie_for_url(args[0], self._clean_cookie) or ""
            elif fn == "limit":
                from core.bandwidth_limiter import bandwidth_limiter
                value = float(bandwidth_limiter.get_rate_bps() or 0)
            elif task is None:
                value = None
            elif fn == "is_archived":
                value = bool(self.archive_manager and self.archive_manager.is_archived(
                    service=task.service, post_id=task.post_id, file_id=task.file_id, file_hash=task.expected_sha256))
            elif fn == "refresh_link":
                value = self._refresh_pawchive_temp_url(task) or ""
            elif fn == "post_process":
                self._post_process_downloaded_file(task, self._options_of(task))
                value = True
            elif fn == "special":
                # Embedded videos (yt-dlp) and Telegram files: the Python download as before
                ok, text = FileDownloader._download_single_file(self, task, None, self._options_of(task))
                value = [bool(ok), str(text)]
        except Exception as e:
            logger.debug(f"Download engine question {fn} failed: {e}", category="downloader")
            if fn == "special":
                value = [False, str(e)]
        self._link.send({"t": "reply", "id": msg.get("id"), "value": value})

    def _options_of(self, task):
        from core.filter_engine import FilterOptions
        return (getattr(task, "options", None) or self.get_batch_options(getattr(task, "batch_id", ""))
                or self.current_options or FilterOptions())

    # ── Controls: Python keeps its own state, Rust is told ──────────────────────

    def _tell(self, obj: Dict[str, Any]) -> None:
        if self._link is not None and self._link.up:
            self._link.send(obj)

    def pause(self):
        super().pause()
        self._tell({"op": "pause"})

    def resume(self):
        super().resume()
        self._tell({"op": "resume"})

    def cancel(self):
        super().cancel()
        self._tell({"op": "cancel"})

    def reset_state(self):
        super().reset_state()
        self._tell({"op": "reset"})

    def pause_batch(self, batch_id: str) -> int:
        n = super().pause_batch(batch_id)
        self._tell({"op": "batch", "batch": batch_id, "paused": True})
        return n

    def _unpause_batch(self, batch_id: str) -> None:
        self._tell({"op": "batch", "batch": batch_id, "paused": False})

    def resume_batch(self, batch_id: str, options=None, cookie_str: str = "") -> int:
        self._unpause_batch(batch_id)
        return super().resume_batch(batch_id, options, cookie_str)

    def cancel_batch(self, batch_id: str, notify: bool = False) -> int:
        self._unpause_batch(batch_id)
        return super().cancel_batch(batch_id, notify)

    def retry_batch_failed(self, batch_id: str, options=None, cookie_str: str = "") -> int:
        self._unpause_batch(batch_id)
        return super().retry_batch_failed(batch_id, options, cookie_str)

    def remove_batch(self, batch_id: str) -> int:
        self._unpause_batch(batch_id)
        return super().remove_batch(batch_id)
