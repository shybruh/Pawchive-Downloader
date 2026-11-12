"""
The Decompressor window runs in the desktop shell (desktop/src-tauri/src/bridge/decompressor.rs):
its list, selecting, the scan, extracting and the password prompts. This part answers what only the
app knows (the settings folder, where 7-Zip is, the Link Vault, the watchlist's folders, the default
download folder, the saved settings) and saves the settings the window changes.
"""

from typing import Any, Dict

from PySide6.QtCore import QObject, Qt, Signal, Slot

from core.archive_password_manager import archive_password_manager
from core.logger import logger
from core.path_translation import localize_path


class DecompressorBridge(QObject):
    _onGui = Signal(object)

    def __init__(self, watchlist_manager, app_bridge=None, parent=None):
        super().__init__(parent)
        self._watchlist_manager = watchlist_manager
        self._app_bridge = app_bridge
        self._settings: Dict[str, Any] = {}
        self._onGui.connect(self._run, Qt.QueuedConnection)
        from services import rust_queue
        if rust_queue._URL and rust_queue._TOKEN:
            link = rust_queue.shared_link()
            link.handlers["decompressor_context"] = self._context
            link.note_handlers["decompressor_settings"] = lambda d: self._onGui.emit(lambda: self._saved(d))
            link.note_handlers["bank_changed"] = lambda _a: self._bank_changed()

    @Slot(object)
    def _run(self, fn):
        fn()

    # ── Saved settings ─────────────────────────────────────────────────────────

    def settings_dict(self) -> Dict[str, Any]:
        return dict(self._settings)

    def load_settings(self, d: Dict[str, Any]) -> None:
        """Settings from settings.json; the window's part takes them when it first asks."""
        self._settings = dict(d) if isinstance(d, dict) else {}

    def _saved(self, d) -> None:
        if not isinstance(d, dict):
            return
        self._settings = dict(d)
        if self._app_bridge is not None and hasattr(self._app_bridge, "saveSettings"):
            try:
                self._app_bridge.saveSettings()
            except Exception as e:
                logger.debug(f"Couldn't save the Decompressor settings: {e}", category="decompressor")

    # ── What the window's part asks ────────────────────────────────────────────

    def _context(self, _args=None) -> Dict[str, Any]:
        from services.bulk_decompressor import get_7za_path
        vault = ""
        try:
            from core.link_vault_manager import link_vault_manager as lvm
            if not getattr(lvm, "_python_wrote", False):
                vault = lvm.store.path
        except Exception:
            pass
        watchlist = []
        for e in list(getattr(self._watchlist_manager, "entries", None) or []):
            watchlist.append({"creator_name": e.creator_name or "", "service": e.service or "",
                              "user_id": str(e.user_id or ""), "download_dir": e.download_dir or ""})
        settings = dict(self._settings)
        settings["custom_locations"] = [
            {"path": localize_path(c["path"]), "creator": c.get("creator", "Custom")}
            for c in settings.get("custom_locations") or [] if isinstance(c, dict) and c.get("path")]
        return {"config_dir": archive_password_manager.config_dir, "seven": get_7za_path() or "", "vault": vault,
                "download_dir": (getattr(self._app_bridge, "_download_dir", "") or "") if self._app_bridge else "",
                "watchlist": watchlist, "settings": settings}

    def _bank_changed(self) -> None:
        """The window changed the password bank: this copy reads it again."""
        apm = archive_password_manager
        try:
            with apm._lock:
                apm._passwords, apm._creator_passwords, apm._archive_map = [], {}, {}
            apm._load()
        except Exception as e:
            logger.debug(f"Couldn't read the password bank again: {e}", category="decompressor")

    def addPassword(self, password: str, creator: str = "Global") -> bool:
        """(the Link Vault's "copy passwords to the Decompressor")"""
        return archive_password_manager.add_password(password, creator)

