"""
Link vault: cloud links (MEGA, Google Drive, Dropbox, GoFile…) and their passwords, saved per
creator and post.

The window's part runs in the desktop shell (desktop/src-tauri/src/bridge/link_vault.rs, the vault
itself is src-tauri/src/vault.rs). This part answers what only the app knows (where the vault is,
the site's login and proxy, whether a site is turned off, the settings folder), copies to the
clipboard, keeps the app's copy of the vault up to date, and adds the links of posts being
downloaded (auto_harvest_posts).
"""

import threading
from typing import Any, Dict, List

from PySide6.QtCore import QObject, Qt, Signal, Slot

from core.logger import logger
from services.link_extractor import LinkExtractor


def _vault():
    from core.link_vault_manager import link_vault_manager
    return link_vault_manager


class LinkVaultBridge(QObject):
    _onGui = Signal(object)

    def __init__(self, app_bridge, parent=None):
        super().__init__(parent)
        self._app = app_bridge
        self._onGui.connect(self._run, Qt.QueuedConnection)
        from services import rust_queue
        if rust_queue._URL and rust_queue._TOKEN:
            link = rust_queue.shared_link()
            link.handlers["vault_context"] = self._context
            link.handlers["vault_clipboard"] = self._clipboard
            link.note_handlers["vault_changed"] = self._changed

    @Slot(object)
    def _run(self, fn):
        fn()

    # ── What the window's part asks ─────────────────────────────────────────

    def _context(self, args) -> Dict[str, Any]:
        url = str((args or [""])[0] or "")
        vm = _vault()
        api = getattr(self._app, "api_client", None)
        site = {"cookie": getattr(api, "cookie_string", "") or "", "user_agent": getattr(api, "user_agent", "") or "",
                "proxy": getattr(api, "proxy_url", "") or ""}
        refused = bool(url) and bool(self._app._refuse_if_disabled(url))
        from core.archive_password_manager import archive_password_manager
        return {"path": "" if getattr(vm, "_python_wrote", False) else vm.store.path, "site": site,
                "refused": refused, "config_dir": archive_password_manager.config_dir}

    def _clipboard(self, args) -> bool:
        text = str((args or [""])[0] or "")
        done = threading.Event()

        def copy():
            try:
                self._app.copyToClipboard(text)
            finally:
                done.set()
        self._onGui.emit(copy)
        done.wait(5)
        return True

    def _changed(self, args) -> None:
        """The window's part changed the vault: the app's copy is read again."""
        vm = _vault()
        rev = (args or [None])[0]
        if rev is None or rev != getattr(vm, "_rust_revision", None):
            vm._rust_revision = rev
            vm._reload_copy()

    # ── Called by downloads ─────────────────────────────────────────────────
    def auto_harvest_posts(self, posts: List[Dict[str, Any]], creator_name: str, domain: str, service: str, user_id: str):
        """Saves the cloud links (and passwords) of posts being downloaded into the vault. Runs in
        the background."""
        if not posts or not service or not user_id:
            return
        posts_copy = list(posts)

        def _worker():
            try:
                ok, r = _vault()._rust("collect", posts=posts_copy, creator_name=creator_name or user_id, domain=domain,
                                       service=service, user_id=user_id, cross=False)
                if ok and isinstance(r, dict):
                    new_links = int(r.get("new") or 0)
                    if new_links > 0:
                        logger.success(f"Link vault: saved {new_links} new link(s) for '{creator_name or user_id}'.", category="vault")
                    return
                harvested = []
                for p in posts_copy:
                    rec = LinkExtractor.extract_post_vault_record(
                        post=p, api_client=None, domain=domain, service=service, user_id=user_id)
                    if rec and rec.get("links"):
                        harvested.append(rec)
                if not harvested:
                    return
                new_links = _vault().add_harvested_data(
                    creator_name=creator_name or user_id, service=service, user_id=user_id, harvested_posts=harvested)
                if new_links > 0:
                    logger.success(f"Link vault: saved {new_links} new link(s) for '{creator_name or user_id}'.", category="vault")
            except Exception as e:
                logger.debug(f"Link vault auto-harvest error: {e}", category="vault")

        threading.Thread(target=_worker, daemon=True).start()
