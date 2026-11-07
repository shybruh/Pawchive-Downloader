"""
Pawchive Downloader — the encrypted credentials vault, as the Python engine still needs it.

The vault (<data>/credentials_vault.enc, DPAPI-encrypted on Windows, AES-256-GCM elsewhere) is
owned by the desktop shell now: the Accounts screen, sign-in, browser import and session tests are
in Rust (desktop/src-tauri/src/accounts/). The Python engine only reads the cookies it sends and
saves the general cookie, so it re-reads the file whenever the shell has changed it, and re-reads
it before it writes, so neither side overwrites the other's changes.
"""

import json
import os
import threading
import time
from typing import Any, Dict, Optional

from core.logger import logger
from core.crypto_utils import encrypt_credential, decrypt_credential


VAULT_CONTEXT = "pawchive_auth_vault"

# Fields that can be sent to a site to authenticate. A saved password is never one of them.
SESSION_FIELDS = ("cookie", "token", "api_key")

# The general session cookie (Downloader → Cookie) is kept in the vault under this name
GENERAL_COOKIE_SLOT = "general"


class AuthManager:
    def __init__(self, data_dir: Optional[str] = None):
        if data_dir:
            base = data_dir
        else:
            from core.path_utils import get_data_dir, migrate_legacy_files
            base = os.path.join(get_data_dir(), "data")
            # Packaged builds used to keep the vault inside "_internal"
            migrate_legacy_files(base, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data"),
                                 ("credentials_vault.enc",))
        os.makedirs(base, exist_ok=True)
        self.vault_file = os.path.join(base, "credentials_vault.enc")
        self._lock = threading.RLock()
        self._data: Dict[str, Any] = {"version": 1, "providers": {}}
        self._stamp = None          # (mtime_ns, size) of the file last read or written
        self.load()
        self._migrate_legacy_settings()

    # ── The file ────────────────────────────────────────────────────────────

    def _file_stamp(self):
        try:
            st = os.stat(self.vault_file)
            return st.st_mtime_ns, st.st_size
        except OSError:
            return None

    def _refresh(self):
        """Re-reads the vault if it changed on disk (the shell saves accounts there)."""
        if self._file_stamp() != self._stamp:
            self.load()

    def load(self):
        """Loads and decrypts the credentials vault from disk."""
        with self._lock:
            self._stamp = self._file_stamp()
            if self._stamp is None:
                self._data = {"version": 1, "providers": {}}
                return
            try:
                with open(self.vault_file, "rb") as f:
                    raw_encrypted = f.read()
                if not raw_encrypted:
                    self._data = {"version": 1, "providers": {}}
                    return
                payload = json.loads(decrypt_credential(raw_encrypted, context=VAULT_CONTEXT).decode("utf-8"))
                self._data = payload if isinstance(payload, dict) and "providers" in payload else {"version": 1, "providers": {}}
                # Older versions kept the general cookie in a "kemono" slot (and had a "coomer" one)
                provs = self._data["providers"]
                migrated = False
                if "kemono" in provs:
                    if GENERAL_COOKIE_SLOT not in provs:
                        provs[GENERAL_COOKIE_SLOT] = provs["kemono"]
                    del provs["kemono"]
                    migrated = True
                if provs.pop("coomer", None) is not None:
                    migrated = True
                from core.crypto_utils import needs_reencrypt
                if migrated or needs_reencrypt(raw_encrypted):
                    self.save()     # move off the old MAC-address key / store the migrated slots
            except Exception as e:
                logger.error(f"[AuthManager] Failed to decrypt credentials vault: {e}", category="auth")
                self._data = {"version": 1, "providers": {}}
                # Keep the unreadable vault instead of overwriting it with an empty one on the next save.
                # Named after its content, so the same vault is copied once (not on every start).
                try:
                    import hashlib
                    import shutil
                    with open(self.vault_file, "rb") as f:
                        digest = hashlib.sha256(f.read()).hexdigest()[:12]
                    backup = self.vault_file + f".unreadable-{digest}"
                    if not os.path.exists(backup):
                        shutil.copy2(self.vault_file, backup)
                    logger.warning(f"[AuthManager] The old credentials vault was kept as {os.path.basename(backup)}.", category="auth")
                except Exception:
                    pass

    def save(self) -> bool:
        """Encrypts and atomically writes the vault."""
        with self._lock:
            try:
                encrypted = encrypt_credential(json.dumps(self._data, indent=2).encode("utf-8"), context=VAULT_CONTEXT)
                tmp_path = self.vault_file + ".tmp"
                with open(tmp_path, "wb") as f:
                    f.write(encrypted)
                    f.flush()
                    try:
                        os.fsync(f.fileno())
                    except OSError:
                        pass
                os.replace(tmp_path, self.vault_file)
                self._stamp = self._file_stamp()
                return True
            except Exception as e:
                logger.error(f"[AuthManager] Failed to save encrypted vault: {e}", category="auth")
                return False

    def _migrate_legacy_settings(self):
        """Moves a plaintext 'cookie' from settings.json (old versions) into the vault."""
        try:
            from core.path_utils import get_config_dir
            from core.atomic_io import atomic_write_json
            candidates = [
                os.path.join(get_config_dir(), "settings.json"),
                os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data", "settings.json"),
            ]
            for settings_path in candidates:
                if not os.path.exists(settings_path):
                    continue
                with open(settings_path, "r", encoding="utf-8") as f:
                    settings_dict = json.load(f)
                legacy_cookie = str(settings_dict.get("cookie", "") or "").strip()
                if not legacy_cookie:
                    continue
                if not self.get_provider_data(GENERAL_COOKIE_SLOT).get("cookie"):
                    self.set_credential(GENERAL_COOKIE_SLOT, {"cookie": legacy_cookie})
                    logger.info("🔒 [AuthManager] Moved the session cookie from settings.json into the encrypted vault.", category="auth")
                settings_dict.pop("cookie", None)
                atomic_write_json(settings_path, settings_dict, indent=2)
        except Exception as e:
            logger.debug(f"[AuthManager] Legacy migration check: {e}", category="auth")

    # ── Reading and writing credentials ─────────────────────────────────────

    def get_provider_data(self, provider_id: str) -> Dict[str, Any]:
        with self._lock:
            self._refresh()
            return dict(self._data.get("providers", {}).get(provider_id, {}))

    def get_credential(self, provider_id: str, field: str = "cookie") -> str:
        """The stored value of one field ("" if not set). For "cookie" a saved token counts too,
        but a password never does."""
        p_data = self.get_provider_data(provider_id)
        val = str(p_data.get(field, "") or "").strip()
        if not val and field == "cookie":
            val = str(p_data.get("token", "") or "").strip()
        return val

    def set_credential(self, provider_id: str, creds: Dict[str, Any]) -> bool:
        """Saves fields for a provider (never a password). Re-reads the vault first."""
        with self._lock:
            self._refresh()
            providers = self._data.setdefault("providers", {})
            curr = providers.get(provider_id, {})
            curr.pop("password", None)
            creds = {k: v for k, v in creds.items() if k != "password"}
            if "cookie" in creds:
                c_val = str(creds["cookie"]).strip()
                if c_val.startswith("eyJ") and "session=" not in c_val:
                    c_val = f"session={c_val}"
                elif c_val and "=" not in c_val and provider_id == GENERAL_COOKIE_SLOT:
                    c_val = f"session={c_val}"
                creds["cookie"] = c_val
            curr.update(creds)
            curr["updated_at"] = int(time.time())
            providers[provider_id] = curr
            return self.save()

    def is_logged_in(self, provider_id: str) -> bool:
        if provider_id == "telegram":
            try:
                from services.telegram_service import TelegramService
                return bool(TelegramService.instance().is_logged_in())
            except Exception:
                return False
        return any(self.get_provider_data(provider_id).get(k) for k in SESSION_FIELDS)


auth_manager = AuthManager()
