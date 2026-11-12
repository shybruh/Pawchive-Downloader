"""MEGA, Dropbox, GoFile and Google Drive links downloaded by the Rust engine
(src-tauri/src/downloader/cloud.rs, gdrive.rs), with the same arguments and log lines as
services/cloud_downloader.py: that one is used when the engine can't be asked (or
PAWCHIVE_PYTHON_CLOUD=1)."""

import os
import threading
from typing import Any, Callable, List, Optional, Union

from core.logger import logger
from services import cloud_downloader as _py
from services import rust_queue


def _engine():
    if os.environ.get("PAWCHIVE_PYTHON_CLOUD", "") == "1" or not rust_queue.available():
        return None
    link = rust_queue.shared_link()
    return link if link.connect() else None


def _run(kind: str, url: str, target_folder: str, log_func: Callable[[str], None], progress_callback, cancel_event,
         pause_event, max_workers: int, skip_words, options, token: str = "") -> Optional[bool]:
    link = _engine()
    if link is None:
        return None
    if isinstance(skip_words, (list, tuple, set)):
        skip_words = ",".join(str(w) for w in skip_words)

    def on_progress(v, *rest):
        if not isinstance(v, dict):
            return
        if "log" in v:
            log_func(str(v["log"]))
        elif progress_callback:
            progress_callback(v.get("file", ""), int(v.get("done") or 0), int(v.get("size") or 0),
                              int(v.get("index") or 1), int(v.get("count") or 1))

    # The pause button: every cloud download pauses together
    stop_watch = threading.Event()

    def watch_pause():
        last = None
        while not stop_watch.wait(0.3):
            now = bool(pause_event and pause_event.is_set())
            if now != last:
                link.send({"op": "cloud_pause", "paused": now})
                last = now
    if pause_event is not None:
        threading.Thread(target=watch_pause, name="CloudPauseWatch", daemon=True).start()
    try:
        ok, v = link.call("cloud_download", {
            "kind": kind, "url": url, "target": target_folder, "workers": int(max_workers or 4),
            "skip_words": str(skip_words or ""), "options": options.to_dict() if hasattr(options, "to_dict") else None,
            "token": token,
        }, progress=on_progress, cancelled=(lambda: cancel_event.is_set()) if cancel_event is not None else None)
    finally:
        stop_watch.set()
    if not ok:
        logger.debug(f"The download engine couldn't download the {kind} link ({v}); downloading it the old way.", category="cloud")
        return None
    return bool(v)


def download_mega_link(url: str, target_folder: str, log_func: Callable[[str], None] = print,
                       progress_callback=None, cancel_event=None, pause_event=None, max_workers: int = 6,
                       skip_words: Optional[Union[str, List[str]]] = None, options: Optional[Any] = None) -> bool:
    r = _run("mega", url, target_folder, log_func, progress_callback, cancel_event, pause_event, max_workers, skip_words, options)
    return r if r is not None else _py.download_mega_link(url, target_folder, log_func, progress_callback, cancel_event,
                                                          pause_event, max_workers, skip_words, options)


def download_dropbox_link(url: str, target_folder: str, log_func: Callable[[str], None] = print,
                          progress_callback=None, cancel_event=None, pause_event=None,
                          skip_words: Optional[Union[str, List[str]]] = None, options: Optional[Any] = None) -> bool:
    r = _run("dropbox", url, target_folder, log_func, progress_callback, cancel_event, pause_event, 1, skip_words, options)
    return r if r is not None else _py.download_dropbox_link(url, target_folder, log_func, progress_callback, cancel_event,
                                                             pause_event, skip_words, options)


def download_gdrive_link(url: str, target_folder: str, log_func: Callable[[str], None] = print,
                         progress_callback=None, cancel_event=None, pause_event=None,
                         skip_words: Optional[Union[str, List[str]]] = None, options: Optional[Any] = None) -> bool:
    r = _run("gdrive", url, target_folder, log_func, progress_callback, cancel_event, pause_event, 1, skip_words, options)
    return r if r is not None else _py.download_gdrive_link(url, target_folder, log_func, progress_callback, cancel_event,
                                                            pause_event, skip_words, options)


def download_gofile_link(url: str, target_folder: str, log_func: Callable[[str], None] = print,
                         progress_callback=None, cancel_event=None, pause_event=None, max_workers: int = 6,
                         skip_words: Optional[Union[str, List[str]]] = None, options: Optional[Any] = None) -> bool:
    # (GoFile Premium's API token, from Settings → Accounts; GoFile's website token is never used)
    from core.auth_manager import auth_manager
    token = auth_manager.get_credential("gofile", "token") or ""
    r = _run("gofile", url, target_folder, log_func, progress_callback, cancel_event, pause_event, max_workers,
             skip_words, options, token)
    return r if r is not None else _py.download_gofile_link(url, target_folder, log_func, progress_callback, cancel_event,
                                                            pause_event, max_workers, skip_words, options)
