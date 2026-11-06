"""
Site accounts (Settings → Accounts): list the sites you can sign in to, save or clear a session
cookie, sign in with a username and password, read a sign-in from an installed browser, and test
a saved session.

Exposed to the UI as "accounts". Everything slow (network, browser files) runs on a worker thread;
its result arrives as a signal carrying the provider id.
"""

import threading

from PySide6.QtCore import QObject, Signal, Slot

from core.logger import logger


class AccountsBridge(QObject):
    providersChanged = Signal()
    providerValidationFinished = Signal(str, bool, str)   # provider id, ok, message
    providerLoginFinished = Signal(str, bool, str)
    providerImportFinished = Signal(str, bool, str)

    @Slot(result=list)
    def getProvidersList(self) -> list:
        from core.auth_manager import auth_manager
        return auth_manager.get_all_providers_summary()

    @Slot(str, str, result=bool)
    def saveProviderCredential(self, providerId: str, credVal: str) -> bool:
        from core.auth_manager import auth_manager, SUPPORTED_PROVIDERS
        auth_type = SUPPORTED_PROVIDERS.get(providerId, {}).get("auth_type", "cookie")
        key = auth_type if auth_type in ("token", "api_key") else "cookie"
        ok = auth_manager.set_credential(providerId, {key: credVal.strip()})
        self.providersChanged.emit()
        return ok

    @Slot(str, result=bool)
    def clearProviderCredential(self, providerId: str) -> bool:
        from core.auth_manager import auth_manager
        ok = auth_manager.clear_credential(providerId)
        self.providersChanged.emit()
        return ok

    @Slot(str)
    def validateProviderSession(self, providerId: str):
        def _worker():
            from core.auth_manager import auth_manager
            ok, msg, _ = auth_manager.validate_session(providerId)
            self.providerValidationFinished.emit(providerId, ok, msg)
            self.providersChanged.emit()

        threading.Thread(target=_worker, daemon=True, name=f"ValWorker_{providerId}").start()

    @Slot(str, str, str)
    def loginProviderWithCredentials(self, providerId: str, username: str, password: str):
        def _worker():
            from core.auth_manager import auth_manager
            ok, msg, _ = auth_manager.login_with_credentials(providerId, username, password)
            self.providerLoginFinished.emit(providerId, ok, msg)
            if ok:
                self.providersChanged.emit()

        threading.Thread(target=_worker, daemon=True, name=f"Login_{providerId}").start()

    @Slot(str)
    @Slot(str, str)
    def importBrowserCookiesToProvider(self, providerId: str, browserId: str = ""):
        """Reads the provider's sign-in from an installed browser; works with Firefox (Chrome, Edge
        and Brave lock their cookies to themselves)."""
        def _worker():
            from services.cookie_importer import browser_cookie_importer
            from core.auth_manager import auth_manager, SUPPORTED_PROVIDERS
            meta = SUPPORTED_PROVIDERS.get(providerId, {})
            domains = [d for d in dict.fromkeys([meta.get("domain", "")] + list(meta.get("mirror_domains", []))) if d] or None
            try:
                res = browser_cookie_importer.import_auto_detect(preferred_browser=browserId or None, domains=domains)
                c_str = res.get("cookie_string", "")
                if not c_str:
                    raise RuntimeError("No sign-in found in your browsers.")
                auth_manager.set_credential(providerId, {"cookie": c_str})
                self.providersChanged.emit()
                self.providerImportFinished.emit(providerId, True, f"Imported from {res.get('browser_name', 'your browser')}")
            except Exception as e:
                logger.error(f"Browser cookie import to {providerId} failed: {e}", category="cookie")
                self.providerImportFinished.emit(providerId, False, str(e))

        threading.Thread(target=_worker, daemon=True, name=f"CookieImport_{providerId}").start()
