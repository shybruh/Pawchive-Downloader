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

Planning (which files of the scanned posts to download, and where) runs in Rust too: the planner
asks here for the Known list, the download archive and video lengths.

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
# The planner's questions, and the questions answered on the reading thread (they wait on nothing
# else from Rust, and planning a big creator asks thousands of them)
_PLAN_ASKS = {"known", "archived", "plan_cancelled"}
_QUICK_ASKS = {"known", "archived", "plan_cancelled", "is_archived", "cookie", "limit"}


def available() -> bool:
    return bool(_URL and _TOKEN) and os.environ.get("PAWCHIVE_PYTHON_DOWNLOADER", "") != "1"


def _key(t) -> str:
    d = t.__dict__
    k = d.get("_rkey")
    if not k:
        k = d["_rkey"] = f"t{next(_keys)}"
    return k


_LEVELS = ("debug", "info", "warning", "error", "success")


def _log(level: str, message: str, to_file: bool = True, category: str = "downloader", details: str = "") -> None:
    if level in ("debug", "info") and not to_file:      # (only these two can stay out of the log file)
        getattr(logger, level)(message, category=category, details=details, to_file=False)
    else:
        getattr(logger, level if level in _LEVELS else "info")(message, category=category, details=details)


class _Link:
    """The socket to the Rust engine: one JSON object per line, both ways. One for the whole app
    (the engine takes one connection): the download queue, the planner and the site client share it."""

    def __init__(self, on_message=None):
        self._on_message = on_message or (lambda msg: None)
        self._sock: Optional[socket.socket] = None
        self._lock = threading.Lock()
        self._connect_lock = threading.Lock()
        self._calls: Dict[int, list] = {}
        self._progress: Dict[int, Any] = {}
        self._call_ids = itertools.count(1)
        self._unreachable_said = False
        self.handlers: Dict[str, Any] = {}        # questions from Rust answered by other parts of the app
        # Told when the engine reports something (a "note": fn -> handler(args), on the message thread)
        self.note_handlers: Dict[str, Any] = {}
        self.on_connect: List[Any] = []           # called after (re)connecting

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
                if not self._unreachable_said:      # (said once: every scan and download tries again)
                    self._unreachable_said = True
                    logger.warning(f"The download engine couldn't be reached ({e}); using the built-in one.",
                                   category="downloader")
                return False
            self._unreachable_said = False
            self._sock = s
            threading.Thread(target=self._read, args=(s,), name="rust-queue-read", daemon=True).start()
        for cb in list(self.on_connect):
            try:
                cb()
            except Exception:
                pass
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

    def new_call_id(self) -> int:
        return next(self._call_ids)

    def call(self, fn: str, args: Dict[str, Any], cid: Optional[int] = None, progress=None, cancelled=None):
        """Runs fn in Rust and waits for its answer: (True, value), or (False, why).
        progress(page, count, total) gets the call's progress notes; when cancelled() turns true,
        Rust is told to stop (the call still returns what was collected)."""
        if threading.current_thread().name == "rust-queue-read":
            # (the answer would come in on this very thread: waiting here would never end)
            return False, "the download engine couldn't be reached from its reading thread"
        cid = cid or self.new_call_id()
        slot = [threading.Event(), (False, "the download engine stopped")]
        self._calls[cid] = slot
        if progress:
            self._progress[cid] = progress
        told = False
        try:
            if not self.send({"op": "call", "id": cid, "fn": fn, "args": args}):
                return False, "the download engine couldn't be reached"
            while not slot[0].wait(0.1 if cancelled else 0.5):
                if self._sock is None:
                    break
                if cancelled and not told and cancelled():
                    told = True
                    self.send({"op": "cancel_call", "id": cid})
            return slot[1]
        finally:
            self._calls.pop(cid, None)
            self._progress.pop(cid, None)

    def _handled_here(self, msg: Dict[str, Any]) -> bool:
        """Messages for any call: results, progress, log lines, account cookies."""
        t = msg.get("t")
        if t == "result":
            self._result(msg)
            return True
        if t == "note":
            fn, args = msg.get("fn"), msg.get("args") or []
            if fn in self.note_handlers:
                try:
                    self.note_handlers[fn](args)
                except Exception as e:
                    logger.debug(f"Engine note {fn!r} failed: {e}", category="downloader")
                return True
            if fn == "log":            # [level, message, to_file, category, details] (the planner's: no category)
                level, message, to_file, category, details = (list(args) + [None] * 5)[:5]
                _log(str(level), str(message), to_file is not False, category or "file", details or "")
                return True
            if fn == "progress":
                cb = self._progress.get(args[0]) if args else None
                if cb:
                    page, count, total = (list(args[1:]) + [None, None, None])[:3]
                    try:
                        if total is None:
                            cb(page, count)
                        else:
                            try:
                                cb(page, count, total)
                            except TypeError:
                                cb(page, count)
                    except Exception as e:
                        logger.debug(f"Progress callback failed: {e}", category="api")
                return True
        if t == "ask" and msg.get("fn") in self.handlers:
            def answer(handler=self.handlers[msg.get("fn")]):
                value = None
                try:
                    value = handler(msg.get("args") or [])
                except Exception as e:
                    value = {"$error": f"{type(e).__name__}: {e}"}
                self.send({"t": "reply", "id": msg.get("id"), "value": value})
            threading.Thread(target=answer, daemon=True).start()
            return True
        if t == "ask" and msg.get("fn") == "account_cookie":
            value = ""
            try:
                from core.auth_manager import auth_manager
                from core.providers import normalize_cookie
                value = normalize_cookie(auth_manager.get_credential((msg.get("args") or [""])[0], "cookie") or "")
            except Exception:
                pass
            self.send({"t": "reply", "id": msg.get("id"), "value": value})
            return True
        return False

    def _result(self, msg: Dict[str, Any]) -> None:
        slot = self._calls.get(msg.get("id"))
        if slot is not None:
            slot[1] = (False, str(msg["error"])) if "error" in msg else (True, msg.get("value"))
            slot[0].set()

    def _close(self, s):
        if self._sock is s:
            self._sock = None
            for slot in list(self._calls.values()):
                slot[0].set()
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
                    if not self._handled_here(msg):
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


_shared: Optional[_Link] = None
_shared_lock = threading.Lock()


def shared_link() -> _Link:
    global _shared
    with _shared_lock:
        if _shared is None:
            _shared = _Link()
        return _shared


class RustDownloader(FileDownloader):
    def __init__(self, *args, **kwargs):
        self._link = None
        self._mw = 4
        super().__init__(*args, **kwargs)
        self._link = shared_link()
        self._link._on_message = self._on_message
        self._by_key: Dict[str, Any] = {}
        self._finished = threading.Event()
        self._result = (False, "")
        self._dirty: Dict[str, Any] = {}
        self._dirty_lock = threading.Lock()
        self._dirty_wake = threading.Event()
        self._clean_cookie = ""
        self._plans: Dict[int, Any] = {}           # call id → "was this plan cancelled?"
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

    # ── Planning: the download tasks of scanned posts ───────────────────────────

    def build_tasks_from_posts(self, posts, creator_name, service, domain, base_dir, options,
                               batch_id=None, artist_dir=None, user_id=""):
        def the_old_way():
            return super(RustDownloader, self).build_tasks_from_posts(
                posts, creator_name, service, domain, base_dir, options,
                batch_id=batch_id, artist_dir=artist_dir, user_id=user_id)

        if os.environ.get("PAWCHIVE_PYTHON_PLANNER", "") == "1" or not self._link.connect():
            return the_old_way()
        from collections import Counter
        from core import folder_naming
        from core.downloader import DownloadTask, MediaTypes

        self._unmatched_known_count = 0
        self.last_build_cancelled = False
        session = self._session_id          # (only a cancel issued during this build counts)
        try:
            new_chars = self.known_manager.find_candidates_in_posts(posts)
        except Exception:
            new_chars = []
        if new_chars:
            logger.debug(
                f"Spotted {len(new_chars)} possible character name(s) in post titles (not added to Known list): "
                f"{', '.join(new_chars[:6])}{'...' if len(new_chars) > 6 else ''}",
                category="known")
        logger.info(f"Structuring download plan for {len(posts)} posts...", category="downloader")

        cid = self._link.new_call_id()
        self._plans[cid] = lambda: self._cancel_event.is_set() and self._session_id != session
        try:
            ok, plan = self._link.call("plan", {
                "posts": posts,
                "options": options.to_dict(),
                "archive": bool(self.archive_manager and self.archive_manager.is_enabled),
                "archive_db": self._archive_db(),
                **self._known_files(),
                "request": {"creator_name": creator_name or "", "service": service or "", "domain": domain or "",
                            "base_dir": base_dir or "", "batch_id": batch_id or "", "artist_dir": artist_dir or "",
                            "user_id": str(user_id or ""), "include_service": bool(folder_naming.include_service)},
            }, cid)
        finally:
            self._plans.pop(cid, None)
        if not ok:
            logger.warning(f"The download engine couldn't plan these posts ({plan}); planning them the old way.",
                           category="downloader")
            return the_old_way()
        return self.tasks_from_plan(plan, options)

    def tasks_from_plan(self, plan: Dict[str, Any], options) -> list:
        """The download tasks of the engine's plan (a "plan" call's answer), with what goes with them:
        post texts to write, harvested links, the "Prepared N files" line"""
        from collections import Counter
        from core.downloader import DownloadTask, MediaTypes
        if plan.get("cancelled"):
            self.last_build_cancelled = True
            logger.info("Task structuring cancelled by user.", category="downloader")
            return []
        self._unmatched_known_count = int(plan.get("unmatched_known") or 0)

        tasks = []
        for d in plan.get("tasks") or []:
            t = DownloadTask(
                url=d["url"], target_path=d["target_path"], post_title=d["post_title"],
                creator_name=d["creator_name"], service=d["service"], post_id=d["post_id"], file_id=d["file_id"],
                file_size=int(d.get("file_size") or 0), expected_sha256=d.get("expected_sha256") or "",
                is_ytdlp=bool(d.get("is_ytdlp")), batch_id=d.get("batch_id") or None,
                post_url=d.get("post_url") or "", post_date=d.get("post_date") or "", user_id=d.get("user_id") or "")
            t.options = options
            t.fallback_urls = list(d.get("fallback_urls") or [])
            t.post_info_path = d.get("post_info_path") or ""
            tasks.append(t)

        # Post texts: text-only posts are saved now; the others once their first file is downloaded
        for path, pid, text in plan.get("pending_post_infos") or []:
            self._post_info_pending[(path, pid)] = text
        for path, pid, text in plan.get("write_now") or []:
            dl._write_post_info(path, text, pid)

        if options.file_type == MediaTypes.LINKS:
            self.harvested_links = {k: list(v) for k, v in (plan.get("harvested_links") or {}).items() if v}
            self.harvested_links_records = list(plan.get("harvested_records") or [])
            total = sum(len(v) for v in self.harvested_links.values())
            if total:
                logger.success(
                    f"🔗 Links scan complete: {total} unique external link(s) found across "
                    f"{len(self.harvested_links)} platform(s). Click 'Download Links' or 'Export Links'.",
                    category="downloader")
            else:
                logger.warning("Links scan complete: No external cloud links were found in any posts.",
                               category="downloader")
        else:
            self.harvested_links = {}
            self.harvested_links_records = []

        not_queued, renamed = Counter(plan.get("not_queued") or {}), Counter(plan.get("renamed") or {})
        notes = [f"Not queued: {dl._counts(not_queued)}." if not_queued else "",
                 f"Renamed to keep files apart: {dl._counts(renamed)}." if renamed else ""]
        logger.success(" ".join([f"Prepared {len(tasks)} file download tasks."] + [n for n in notes if n]),
                       category="downloader")
        return tasks

    def _plan_answer(self, fn: str, args: List[Any]) -> Any:
        if fn == "plan_cancelled":
            check = self._plans.get(args[0])
            return bool(check()) if check else False
        if fn == "known":
            title, tags, filenames, content = (list(args) + [None] * 4)[:4]
            hit = self.known_manager.find_matching_hierarchy(title, tags=tags, filenames=filenames, content=content)
            return [hit[0] or "", hit[1] or ""] if hit else None
        if fn == "archived":
            service, post_id, file_id, sha = args
            return bool(self.archive_manager and self.archive_manager.is_archived(
                service=service, post_id=post_id, file_id=file_id, file_hash=sha))
        return None

    def _known_files(self) -> Dict[str, str]:
        """Known.txt and the learned franchise names, for the engine (it matches posts itself)"""
        km = self.known_manager
        path = getattr(km, "file_path", "") if km else ""
        if not path or not os.path.exists(path):
            return {}                            # (then the engine asks here)
        return {"known_txt": path,
                "franchise_cache": getattr(getattr(km, "franchise_resolver", None), "_cache_path", "") or ""}

    def _archive_db(self) -> str:
        """The download archive's file, for the engine (it looks files up and records them itself);
        "" when the archive is off or not open"""
        am = self.archive_manager
        if not (am and am.is_enabled) or not getattr(am, "db_path", "") or not hasattr(am, "_init_db"):
            return ""                            # (then the engine asks here)
        if getattr(am, "_conn", None) is None:
            am._init_db()                        # (the engine needs the tables to exist)
        return am.db_path if getattr(am, "_conn", None) is not None else ""

    def _note(self, fn: str, args: List[Any]) -> None:
        try:
            if fn == "archive_changed":
                if self.archive_manager:
                    self.archive_manager._stats_cache = None
            elif fn == "archive_error":
                message, error = (list(args) + ["", ""])[:2]
                logger.warning(str(message), category="archive")
                if self.archive_manager:
                    self.archive_manager._note_error(Exception(str(error)))
            elif fn == "record_link":
                if self.archive_manager and self.archive_manager.is_enabled:
                    service, creator_id, post_id, url, creator_name, post_title, link_title = args
                    self.archive_manager.record_link(
                        service=service, creator_id=creator_id, post_id=post_id, url=url,
                        creator_name=creator_name, post_title=post_title, link_title=link_title)
            elif fn in ("archive_password", "add_passwords"):
                from core.archive_password_manager import archive_password_manager
                if fn == "archive_password":
                    archive_password_manager.record_archive_password(args[0], args[1])
                else:
                    archive_password_manager.add_passwords(list(args[0]))
        except Exception as e:
            logger.debug(f"Download engine note {fn} failed: {e}", category="downloader")

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
        self._link.send({"op": "start", "options": options.to_dict(), "archive": archive, "workers": self._mw,
                         "archive_db": self._archive_db()})
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
            if msg.get("fn") in _QUICK_ASKS:
                self._answer(msg)
            else:
                threading.Thread(target=self._answer, args=(msg,), daemon=True).start()
        elif t == "note":
            self._note(msg.get("fn"), msg.get("args") or [])
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
            if fn in _PLAN_ASKS:
                value = self._plan_answer(fn, args)
            elif fn == "cookie":
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
