"""
The watchlist's database (watchlist.db) kept by the desktop shell's Rust port
(desktop/src-tauri/src/watchlist/store.rs).

RustWatchlistStore has WatchlistStore's interface, so the WatchlistManager doesn't change: each
operation is a call to the engine. If the engine can't be reached, the Python store answers
(both use the same SQLite file and its locking).
"""

import sqlite3
from typing import Any, Dict, Iterable, List, Optional, Tuple

from core.logger import logger
from core.watchlist_store import WatchlistStore
from services import rust_queue

_FAILED = object()


class RustWatchlistStore(WatchlistStore):
    def _call(self, op: str, **args) -> Any:
        """(ok, value): ok False with the error text when Rust answered with an error;
        _FAILED when Rust couldn't be asked."""
        link = rust_queue.shared_link()
        if not link.connect():
            return _FAILED
        args.update(op=op, path=self.path)
        ok, value = link.call("watchlist_store", args)
        if not ok and ("engine" in str(value) and ("stopped" in str(value) or "reached" in str(value))):
            return _FAILED              # (the connection, not the database)
        return ok, value

    def has_rows(self) -> bool:
        r = self._call("has_rows")
        if r is _FAILED:
            return super().has_rows()
        ok, v = r
        if not ok:
            raise sqlite3.DatabaseError(v)
        return bool(v)

    def load(self) -> List[Tuple[int, int, Dict[str, Any], List[Dict[str, Any]]]]:
        r = self._call("load")
        if r is _FAILED:
            return super().load()
        ok, v = r
        if not ok:
            raise sqlite3.DatabaseError(v)
        return [(int(rid), int(pos), d, list(posts or [])) for rid, pos, d, posts in v]

    def get_meta(self, key: str, default: str = "") -> str:
        r = self._call("get_meta", key=key, default=default)
        if r is _FAILED:
            return super().get_meta(key, default)
        ok, v = r
        if not ok:
            raise sqlite3.DatabaseError(v)
        return v

    def set_meta(self, key: str, value: str) -> None:
        r = self._call("set_meta", key=key, value=value)
        if r is _FAILED:
            return super().set_meta(key, value)
        ok, v = r
        if not ok:
            raise sqlite3.DatabaseError(v)

    def write(self, rows: Iterable[Tuple[int, int, Dict[str, Any], Dict[str, Any], List[Dict[str, Any]]]],
              deleted: Iterable[int] = (), replace_all: bool = False) -> int:
        rows, deleted = [list(r) for r in rows], [int(d) for d in deleted]
        r = self._call("write", rows=rows, deleted=deleted, replace_all=bool(replace_all))
        if r is _FAILED:
            return super().write(rows, deleted, replace_all)
        ok, v = r
        if not ok:
            raise sqlite3.DatabaseError(v)
        return int(v)

    def drop_unused_options(self) -> None:
        r = self._call("drop_unused_options")
        if r is _FAILED:
            return super().drop_unused_options()
        ok, v = r
        if not ok:
            raise sqlite3.DatabaseError(v)

    def backup(self, dest: Optional[str] = None) -> Optional[str]:
        r = self._call("backup", dest=dest)
        if r is _FAILED:
            return super().backup(dest)
        ok, v = r
        if not ok:
            logger.warning(f"Couldn't back up the watchlist: {v}", category="watchlist")
            return None
        return v

    def set_aside(self) -> Optional[str]:
        r = self._call("set_aside")
        if r is _FAILED:
            return super().set_aside()
        ok, v = r
        if not ok:
            logger.warning(str(v), category="watchlist")
            return None
        return v
