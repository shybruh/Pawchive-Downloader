"""
The archive sites' client (core/api_client.py) answered by the desktop shell's Rust port
(desktop/src-tauri/src/downloader/sites.rs).

RustApiClient is the app's ApiClient with its site requests made in Rust: creator profiles and
names, post lists, DMs, single posts (with Pawchive's temporary links), comments, creator tags,
account favorites and the whole-site creator lists. Its log lines, progress and cancelling work
as before. The cookie, user agent and proxy set here go along with every request.

Without the desktop shell (or with PAWCHIVE_PYTHON_SITES=1), or if the engine can't be reached,
the Python client answers.
"""

import os
from typing import Any, Dict, List, Optional

from core.api_client import ApiClient
from services import rust_queue

_FAILED = object()


def available() -> bool:
    return rust_queue.available() and os.environ.get("PAWCHIVE_PYTHON_SITES", "") != "1"


def _parsed(p) -> Dict[str, Any]:
    def safe(v):
        return v if isinstance(v, (str, int, float, bool, type(None), list, dict)) else str(v)
    return {
        "domain": p.domain or "", "service": p.service or "", "user_id": str(p.user_id or ""),
        "post_id": str(p.post_id) if p.post_id else None, "provider": p.provider or "archive",
        "is_single_post": bool(p.is_single_post), "is_dms": bool(getattr(p, "is_dms", False)),
        "is_valid": bool(p.is_valid), "raw_url": p.raw_url or "", "error_msg": p.error_msg or "",
        "extra_data": {str(k): safe(v) for k, v in (p.extra_data or {}).items()},
    }


class RustApiClient(ApiClient):
    def _site(self, fn: str, progress=None, cancelled=None, **args) -> Any:
        """The Rust client's answer, or _FAILED (then the Python client is used)."""
        link = rust_queue.shared_link()
        if not link.connect():
            return _FAILED
        args.update(fn=fn, cookie=self.cookie_string or "", user_agent=self.user_agent or "", proxy=self.proxy_url or "")
        ok, value = link.call("site", args, progress=progress, cancelled=cancelled)
        if not ok:
            from core.logger import logger
            logger.debug(f"The download engine couldn't answer {fn} ({value}); asking the old way.", category="api")
            return _FAILED
        return value

    @staticmethod
    def _stop(cancel_event):
        return (lambda: cancel_event.is_set()) if cancel_event is not None else None

    # ── Creators ─────────────────────────────────────────────────────────────

    def fetch_creator_profile(self, parsed) -> Dict[str, Any]:
        v = self._site("profile", link=_parsed(parsed))
        return super().fetch_creator_profile(parsed) if v is _FAILED else v

    def resolve_creator_name(self, parsed) -> Optional[str]:
        if not parsed or not parsed.is_valid:
            return None
        v = self._site("creator_name", link=_parsed(parsed))
        return super().resolve_creator_name(parsed) if v is _FAILED else v

    def fetch_creator_tags(self, parsed) -> List[str]:
        v = self._site("tags", link=_parsed(parsed))
        return super().fetch_creator_tags(parsed) if v is _FAILED else list(v or [])

    def creator_listing_cost(self, domain: str) -> int:
        v = self._site("listing_cost", domain=domain or "")
        return super().creator_listing_cost(domain) if v is _FAILED else int(v or 0)

    def fetch_creator_updates(self, domain: str, max_age: float = 600.0, should_stop=None):
        v = self._site("creator_updates", cancelled=should_stop, domain=domain or "", max_age=float(max_age))
        if v is _FAILED:
            return super().fetch_creator_updates(domain, max_age=max_age, should_stop=should_stop)
        if v is None or (should_stop and should_stop()):
            return None
        return {(svc, uid): t for svc, uid, t in v}

    # ── Posts ────────────────────────────────────────────────────────────────

    def resolve_pawchive_deferred_attachments(self, post: Dict[str, Any], domain: str = "pawchive.pw") -> int:
        v = self._site("resolve_links", post=post, domain=domain or "pawchive.pw")
        if v is _FAILED:
            return super().resolve_pawchive_deferred_attachments(post, domain=domain)
        post.clear()
        post.update(v.get("post") or {})
        return int(v.get("resolved") or 0)

    def fetch_single_post(self, parsed) -> Optional[Dict[str, Any]]:
        if not parsed.post_id:
            return None
        v = self._site("single_post", link=_parsed(parsed))
        return super().fetch_single_post(parsed) if v is _FAILED else v

    def fetch_post_comments(self, domain: str, service: str, user_id: str, post_id: str) -> List[Dict[str, Any]]:
        v = self._site("comments", domain=domain or "", service=service or "", user_id=str(user_id or ""), post_id=str(post_id or ""))
        return super().fetch_post_comments(domain, service, user_id, post_id) if v is _FAILED else list(v or [])

    def fetch_user_posts(self, parsed, page_start: int = 1, page_end: int = 999999, page_size: int = 50,
                         progress_callback=None, cancel_event=None, date_after: str = "", date_before: str = ""):
        v = self._site("posts", progress=progress_callback, cancelled=self._stop(cancel_event), link=_parsed(parsed),
                       page_start=int(page_start), page_end=int(page_end), page_size=int(page_size),
                       date_after=date_after or "", date_before=date_before or "")
        if v is _FAILED:
            return super().fetch_user_posts(parsed, page_start, page_end, page_size, progress_callback, cancel_event,
                                            date_after, date_before)
        self._fetch_state.failed = bool(v.get("failed"))
        return list(v.get("posts") or [])

    def fetch_user_dms(self, parsed, page_start: int = 1, page_end: int = 999999, page_size: int = 50,
                       progress_callback=None, cancel_event=None, date_after: str = "", date_before: str = ""):
        v = self._site("dms", progress=progress_callback, cancelled=self._stop(cancel_event), link=_parsed(parsed),
                       page_start=int(page_start), page_end=int(page_end), page_size=int(page_size))
        if v is _FAILED:
            return super().fetch_user_dms(parsed, page_start, page_end, page_size, progress_callback, cancel_event,
                                          date_after, date_before)
        return list(v or [])

    def fetch_user_favorites(self, domain: str = "pawchive.pw", fav_type: str = "post", page_start: int = 1,
                             page_end: int = 999999, page_size: int = 50, progress_callback=None, cancel_event=None):
        v = self._site("favorites", progress=progress_callback, cancelled=self._stop(cancel_event), domain=domain or "",
                       fav_type=fav_type or "post", page_start=int(page_start), page_end=int(page_end),
                       page_size=int(page_size))
        if v is _FAILED:
            return super().fetch_user_favorites(domain, fav_type, page_start, page_end, page_size, progress_callback,
                                                cancel_event)
        return list(v or [])
