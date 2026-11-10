"""
The watchlist's check for new posts run by the desktop shell's Rust engine
(desktop/src-tauri/src/watchlist/check.rs): the round (sites in parallel, resuming a stopped
round, skipping creators a site's list says haven't posted), each creator's new posts and the
look for files already downloaded. Each creator's result reaches the watchlist (and the window)
as soon as it's known.

RustWatchlistChecker is core/watchlist_checker.py's WatchlistChecker with its run done in Rust;
when the engine can't be asked, the Python one runs.
"""

from typing import Any, Iterable

from core import watchlist_manager as wm
from core.watchlist_checker import WatchlistChecker


def site_args(api_client) -> dict:
    """The site client's settings, for the engine's own"""
    return {"cookie": getattr(api_client, "cookie_string", "") or "",
            "user_agent": getattr(api_client, "user_agent", "") or "",
            "proxy": getattr(api_client, "proxy_url", "") or ""}


class RustWatchlistChecker(WatchlistChecker):
    def __init__(self, *args, api_client=None, **kwargs):
        super().__init__(*args, **kwargs)
        self.api_client = api_client

    def run(self, entries: Iterable[Any], resume: bool = True) -> int:
        entries = list(entries)
        r = self.manager.check_in_rust(entries, site=site_args(self.api_client), resume=resume,
                                       progress=self._progress_from_rust, cancelled=self.cancel_event.is_set)
        if r is wm._NO:
            return super().run(entries, resume)
        self.total_new = int(r.get("total_new") or 0)
        self.failed = int(r.get("failed") or 0)
        self.skipped_unchanged = int(r.get("skipped_unchanged") or 0)
        self.done, self.total = int(r.get("done") or 0), int(r.get("total") or 0)
        self._progress()
        return self.total_new

    def _progress_from_rust(self, done, total=None, *_):
        self.done, self.total = int(done or 0), int(total or 0)
        self._progress()
