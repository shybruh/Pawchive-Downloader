"""
Hands a file's transfer to the desktop shell's download engine (desktop/src-tauri/src/transfer/).

The engine downloads in 8 MB chunks it records as they finish, so an interrupted, paused or
crashed download continues from the last chunk that's verifiably on disk, and a dropped
connection retries on its own. The Python downloader still picks the server and headers, checks
filters and gives the finished file its name.
"""

import json
import os
import socket
from typing import Callable, Dict, Optional

# "tcp://127.0.0.1:<port>", set by the desktop shell when it starts the engine
_URL = os.environ.get("PAWCHIVE_TRANSFER_URL", "")
_TOKEN = os.environ.get("PAWCHIVE_TRANSFER_TOKEN", "")


def available() -> bool:
    return bool(_URL and _TOKEN)


class Sha256Result:
    """Stands in for hashlib's object where the downloader expects one (hexdigest())."""

    def __init__(self, hexdigest: str):
        self._hex = hexdigest

    def hexdigest(self) -> str:
        return self._hex


def transfer(url: str, part_path: str, headers: Dict[str, str], total: int, ranges: bool, connections: int,
             restart: bool, want_hash: bool, limit_bps: float,
             on_progress: Optional[Callable[[int, int], None]] = None,
             should_stop: Optional[Callable[[], Optional[str]]] = None) -> dict:
    """Downloads `url` into `part_path`. Returns the engine's final answer:
    {"ok": True, "size", "sha256", "resumedFrom"} or {"ok": False, "kind", "status", "error"}.

    on_progress(done, total) is called about four times a second. should_stop() returning a
    reason ("Cancelled", "Batch paused by user", …) stops the transfer; what's on disk is kept."""
    job = {
        "url": url, "part_path": part_path, "headers": headers, "total": int(total or 0),
        "ranges": bool(ranges), "connections": int(max(1, connections)), "restart": bool(restart),
        "hash": bool(want_hash), "limit_bps": float(limit_bps or 0),
    }
    host, _, port = _URL.replace("tcp://", "").rpartition(":")
    try:
        sock = socket.create_connection((host, int(port)), timeout=10)
    except (OSError, ValueError) as e:
        return {"ok": False, "kind": "other", "status": 0, "error": f"The download engine didn't answer: {e}"}
    try:
        sock.settimeout(90)          # the engine sends progress four times a second
        sock.sendall((json.dumps({"token": _TOKEN, "job": job}) + "\n").encode("utf-8"))
        reader = sock.makefile("r", encoding="utf-8")
        for line in reader:
            line = line.strip()
            if not line:
                continue
            msg = json.loads(line)
            if msg.get("t") == "done":
                return msg
            if on_progress:
                on_progress(int(msg.get("done", 0)), int(msg.get("total", 0)))
            reason = should_stop() if should_stop else None
            if reason:
                return {"ok": False, "kind": "stopped", "status": 0, "error": reason}
        return {"ok": False, "kind": "network", "status": 0, "error": "The download engine stopped answering"}
    except (OSError, ValueError) as e:
        return {"ok": False, "kind": "network", "status": 0, "error": f"Lost the download engine: {e}"}
    finally:
        try:
            sock.close()     # also tells the engine to stop if we're leaving early
        except OSError:
            pass
