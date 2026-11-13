"""The album sites (Bunkr, Erome, nHentai) read by the Rust engine (src-tauri/src/downloader/albums.rs),
with the same answers as services/bunkr_client.py, erome_client.py and nhentai_client.py: those
are used when the engine can't be asked (or PAWCHIVE_PYTHON_ALBUMS=1)."""

import os
from typing import Any, Dict, List, Optional, Tuple

from core.logger import logger
from services import rust_queue
from services.bunkr_client import fetch_bunkr_album as _py_bunkr
from services.erome_client import fetch_erome_album as _py_erome
from services.nhentai_client import fetch_nhentai_gallery as _py_nhentai

Album = Tuple[Optional[str], List[Dict[str, Any]]]


def _ask(kind: str, url: str) -> Optional[Album]:
    if os.environ.get("PAWCHIVE_PYTHON_ALBUMS", "") == "1" or not rust_queue.available():
        return None
    link = rust_queue.shared_link()
    if not link.connect():
        return None
    ok, v = link.call("album_files", {"kind": kind, "url": str(url or "")})
    if not ok or not isinstance(v, dict):
        logger.debug(f"The download engine couldn't read the {kind} link ({v}); reading it the old way.", category="api")
        return None
    return v.get("title"), list(v.get("files") or [])


def fetch_bunkr_album(url: str, headers: Optional[dict] = None, timeout: int = 25, resolve_files: bool = True) -> Album:
    if headers is None and resolve_files:
        r = _ask("bunkr", url)
        if r is not None:
            return r
    return _py_bunkr(url, headers=headers, timeout=timeout, resolve_files=resolve_files)


def fetch_erome_album(url: str, headers: Optional[dict] = None, timeout: int = 25) -> Album:
    if headers is None:
        r = _ask("erome", url)
        if r is not None:
            return r
    return _py_erome(url, headers=headers, timeout=timeout)


def fetch_nhentai_gallery(gallery_id_or_url: str, headers: Optional[dict] = None, timeout: int = 25) -> Album:
    if headers is None:
        r = _ask("nhentai", gallery_id_or_url)
        if r is not None:
            return r
    return _py_nhentai(gallery_id_or_url, headers=headers, timeout=timeout)


def album_tasks(provider: str, url: str, post_id: str, download_dir: str):
    """(creator name, task fields) of an album link's files, read and named by the engine
    (src-tauri/src/downloader/scan.rs album_tasks), or None when it can't be asked."""
    if os.environ.get("PAWCHIVE_PYTHON_ALBUMS", "") == "1" or not rust_queue.available():
        return None
    link = rust_queue.shared_link()
    if not link.connect():
        return None
    ok, v = link.call("album_tasks", {"kind": provider, "url": str(url or ""), "post_id": str(post_id or ""),
                                      "base_dir": str(download_dir or "")})
    if not ok or not isinstance(v, dict):
        logger.debug(f"The download engine couldn't read the {provider} link ({v}); reading it the old way.", category="api")
        return None
    return str(v.get("creator_name") or ""), list(v.get("tasks") or [])
