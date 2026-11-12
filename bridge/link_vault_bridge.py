"""
Link vault: cloud links (MEGA, Google Drive, Dropbox, GoFile…) and their passwords, saved per
creator and post. Collect a creator's links, check which still work, edit passwords, remove or
export them.

Exposed to the UI as "linkVault". Downloads also add to it as they run (auto_harvest_posts).
Downloading the links themselves is cloud-download work and stays with the downloads code.
"""

import os
import threading
from typing import Any, Dict, List

from PySide6.QtCore import QObject, Property, Qt, QUrl, Signal, Slot

from bridge.record_list_model import RecordListModel
from core.logger import logger
from core.parser import LinkParser
from core.text_utils import clean_text
from services.link_extractor import LinkExtractor


def _vault():
    from core.link_vault_manager import link_vault_manager
    return link_vault_manager


class LinkVaultBridge(QObject):
    changed = Signal()
    probingFinished = Signal()
    harvestFinished = Signal(bool, str, int, int)     # ok, creator name, new links, posts scanned
    asyncResultReady = Signal(str, "QVariant")
    _callOnGui = Signal(object)                        # run a function on the window thread

    def __init__(self, app_bridge, parent=None):
        super().__init__(parent)
        self._app = app_bridge          # site client, archive, clipboard, decompressor
        self._creators_model = RecordListModel(self, key_field="creator_key")
        self._search = ""
        self._platform = "all"
        self._tree_cache = None
        self._harvesting = False
        self._harvest_status = ""
        self._harvest_cancel = threading.Event()
        self._callOnGui.connect(lambda fn: fn(), Qt.QueuedConnection)

    # ── What the screen shows ───────────────────────────────────────────────
    @Property(QObject, constant=True)
    def creatorsModel(self) -> RecordListModel:
        """The creators for the current search (filled by treeAsync)."""
        return self._creators_model

    @Property(int, notify=changed)
    def totalLinks(self) -> int:
        return len(_vault().data.get("links", []))

    @Property(int, notify=changed)
    def totalCreators(self) -> int:
        return len(_vault().data.get("creators", {}))

    @Property(bool, notify=changed)
    def probingActive(self) -> bool:
        return _vault()._probing_active

    @Property(bool, notify=changed)
    def harvestingActive(self) -> bool:
        return self._harvesting

    @Property(str, notify=changed)
    def harvestingStatus(self) -> str:
        return self._harvest_status

    def _tree(self) -> list:
        """The vault's tree for the current search / platform, built once per vault change."""
        vm = _vault()
        key = (getattr(vm, "revision", 0), len(vm.data.get("links", [])), self._search, self._platform)
        if self._tree_cache and self._tree_cache[0] == key:
            return self._tree_cache[1]
        tree = vm.get_tree_model(self._search, self._platform)
        self._tree_cache = (key, tree)
        return tree

    @Slot(str, str)
    def refresh(self, search: str = "", platform: str = ""):
        self._search = search
        self._platform = platform
        self.changed.emit()

    @Slot(str)
    def treeAsync(self, request_id: str):
        """Builds the creators in the background into creatorsModel (the screen shows them with
        applyPending()); asyncResultReady answers with their number."""
        def _job():
            try:
                tree = self._tree()
            except Exception as e:
                logger.error(f"Link vault: building the list failed: {e}", category="vault")
                tree = []

            def _deliver():
                self._creators_model.set_pending(tree)
                self.asyncResultReady.emit(request_id, {"count": len(tree)})
            try:
                self._callOnGui.emit(_deliver)
            except RuntimeError:
                pass      # the app is closing
        threading.Thread(target=_job, daemon=True, name=f"async:{request_id[:24]}").start()

    # ── Editing ─────────────────────────────────────────────────────────────
    @Slot(str, result=bool)
    def deleteLink(self, linkId: str) -> bool:
        ok = _vault().delete_link(linkId)
        if ok:
            self.changed.emit()
        return ok

    @Slot(str, "QVariant", result=bool)
    def updateLinkPasswords(self, linkId: str, passwords) -> bool:
        if isinstance(passwords, str):
            pw_list = [p.strip() for p in passwords.split(",") if p.strip()]
        elif isinstance(passwords, (list, tuple)):
            pw_list = [str(p).strip() for p in passwords if str(p).strip()]
        else:
            pw_list = []
        ok = _vault().update_link_passwords(linkId, pw_list)
        if ok:
            self.changed.emit()
        return ok

    @Slot(str, result=bool)
    def deletePost(self, postId: str) -> bool:
        ok = _vault().delete_post(postId)
        if ok:
            self.changed.emit()
        return ok

    @Slot(str, result=bool)
    def deleteCreator(self, creatorKey: str) -> bool:
        ok = _vault().delete_creator(creatorKey)
        if ok:
            self.changed.emit()
        return ok

    @Slot(result=int)
    def cleanDeadLinks(self) -> int:
        count = _vault().clean_dead_links()
        self.changed.emit()
        return count

    # ── Copy / export ───────────────────────────────────────────────────────
    @Slot(str, result=int)
    @Slot(result=int)
    def copyLinks(self, creatorKey: str = "") -> int:
        urls = _vault().get_all_link_urls(creatorKey)
        if urls:
            self._app.copyToClipboard("\n".join(urls))
        return len(urls)

    @Slot(str, result=int)
    def exportToFolder(self, folderPath: str) -> int:
        path = folderPath or ""
        if path.startswith("file:"):
            path = QUrl(path).toLocalFile()
        return _vault().export_all_to_folder(os.path.normpath(path))

    @Slot(result=int)
    def copyPasswordsToDecompressor(self) -> int:
        pws = _vault().get_all_passwords()
        dec = getattr(self._app, "_decompressor_bridge", None)
        if dec is not None and hasattr(dec, "addPassword"):
            for pw in pws:
                if pw:
                    dec.addPassword(pw)
        logger.success(f"Copied {len(pws)} password(s) from the link vault to the Decompressor.", category="vault")
        return len(pws)

    # ── Checking which links still work ─────────────────────────────────────
    def _probe(self, cancel_event=None, creator_key=None):
        def _progress(done, total, url):
            self.changed.emit()

        def _complete():
            self.changed.emit()
            self.probingFinished.emit()
        kwargs = {"progress_callback": _progress, "completion_callback": _complete}
        if cancel_event is not None:
            kwargs["cancel_event"] = cancel_event
        if creator_key:
            kwargs["creator_key"] = creator_key
        _vault().probe_all_links_async(**kwargs)

    @Slot()
    def probeHealth(self):
        self._probe()

    # ── Collecting a creator's links ────────────────────────────────────────
    @Slot(str, bool, int, int)
    def harvestCreator(self, url: str, probeHealth: bool = True, pageStart: int = 1, pageEnd: int = 999999):
        clean_url = (url or "").strip()
        if not clean_url:
            self.harvestFinished.emit(False, "", 0, 0)
            return
        if self._harvesting:
            logger.warning("The link vault is already collecting a creator's links.", category="vault")
            return

        def _worker():
            self._harvesting = True
            self._harvest_cancel.clear()
            self._harvest_status = "Connecting..."
            self.changed.emit()
            api = self._app.api_client
            try:
                parsed = LinkParser.parse(clean_url)
                if not parsed.is_valid:
                    logger.error(f"Link vault: not a creator link: '{clean_url}'", category="vault")
                    self.harvestFinished.emit(False, "Invalid URL", 0, 0)
                    return
                if self._app._refuse_if_disabled(clean_url):
                    self.harvestFinished.emit(False, "Site turned off", 0, 0)
                    return

                profile = api.fetch_creator_profile(parsed)
                creator_name = clean_text(profile.get("name", parsed.user_id) or parsed.user_id)
                creator_key = f"{parsed.service}:{parsed.user_id}".lower()
                self._harvest_status = f"Scanning posts for {creator_name}..."
                self.changed.emit()

                if parsed.is_single_post:
                    single = api.fetch_single_post(parsed)
                    posts = [single] if single else []
                elif parsed.is_dms:
                    posts = api.fetch_user_dms(parsed=parsed, page_start=pageStart, page_end=pageEnd,
                                               cancel_event=self._harvest_cancel)
                else:
                    posts = api.fetch_user_posts(parsed=parsed, page_start=pageStart, page_end=pageEnd,
                                                 cancel_event=self._harvest_cancel)

                if self._harvest_cancel.is_set():
                    logger.warning(f"Link vault: stopped while reading posts of {creator_name}.", category="vault")
                    self.harvestFinished.emit(False, creator_name, 0, len(posts))
                    return
                if not posts:
                    logger.info(f"Link vault: no posts found for {creator_name}.", category="vault")
                    self.harvestFinished.emit(True, creator_name, 0, 0)
                    return

                vm = _vault()
                site = None
                try:
                    from services.rust_sites import RustApiClient
                    if isinstance(api, RustApiClient):
                        site = {"cookie": api.cookie_string or "", "user_agent": api.user_agent or "", "proxy": api.proxy_url or ""}
                except Exception:
                    pass

                def _progress(v, *rest):
                    if isinstance(v, dict) and "done" in v:
                        self._harvest_status = f"Processed {v['done']}/{v.get('total', len(posts))} posts ({v.get('links', 0)} links found)"
                        self.changed.emit()
                ok, r = (vm._rust("collect", progress=_progress, cancelled=self._harvest_cancel.is_set, posts=posts,
                                  creator_name=creator_name, domain=parsed.domain, service=parsed.service,
                                  user_id=parsed.user_id, cross=True, site=site)
                         if site is not None else (False, None))
                if ok and isinstance(r, dict):
                    if r.get("cancelled") or self._harvest_cancel.is_set():
                        logger.warning(f"Link vault: stopped while reading links of {creator_name}.", category="vault")
                        self.harvestFinished.emit(False, creator_name, 0, len(posts))
                        return
                    new_links = int(r.get("new") or 0)
                    self.changed.emit()
                    if probeHealth and new_links > 0 and not self._harvest_cancel.is_set():
                        self._harvest_status = f"Probing links for {creator_name}..."
                        self.changed.emit()
                        self._probe(cancel_event=self._harvest_cancel, creator_key=creator_key)
                    logger.success(f"Link vault: {creator_name} done ({new_links} new links from {len(posts)} posts).", category="vault")
                    self.harvestFinished.emit(True, creator_name, new_links, len(posts))
                    return

                harvested, total_links = [], 0
                for idx, p in enumerate(posts):
                    if self._harvest_cancel.is_set():
                        break
                    rec = LinkExtractor.extract_post_vault_record(
                        post=p, api_client=api, domain=parsed.domain, service=parsed.service, user_id=parsed.user_id)
                    if rec and rec.get("links"):
                        harvested.append(rec)
                        total_links += len(rec["links"])
                    if (idx + 1) % 10 == 0 or idx == len(posts) - 1:
                        self._harvest_status = f"Processed {idx + 1}/{len(posts)} posts ({total_links} links found)"
                        self.changed.emit()

                if self._harvest_cancel.is_set():
                    logger.warning(f"Link vault: stopped while reading links of {creator_name}.", category="vault")
                    self.harvestFinished.emit(False, creator_name, 0, len(posts))
                    return

                new_links = _vault().add_harvested_data(
                    creator_name=creator_name, service=parsed.service, user_id=parsed.user_id, harvested_posts=harvested)
                self.changed.emit()

                if probeHealth and new_links > 0 and not self._harvest_cancel.is_set():
                    self._harvest_status = f"Probing links for {creator_name}..."
                    self.changed.emit()
                    self._probe(cancel_event=self._harvest_cancel, creator_key=creator_key)

                logger.success(f"Link vault: {creator_name} done ({new_links} new links from {len(posts)} posts).", category="vault")
                self.harvestFinished.emit(True, creator_name, new_links, len(posts))
            except Exception as e:
                logger.error(f"Link vault: collecting links failed: {e}", category="vault")
                self.harvestFinished.emit(False, str(e), 0, 0)
            finally:
                self._harvesting = False
                self._harvest_status = ""
                self.changed.emit()

        threading.Thread(target=_worker, daemon=True, name="VaultArtistHarvester").start()

    @Slot()
    def cancelHarvest(self):
        self._harvest_cancel.set()
        self._harvesting = False
        self._harvest_status = "Cancelled"
        self.changed.emit()

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
                        self.changed.emit()
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
                    self.changed.emit()

            except Exception as e:
                logger.debug(f"Link vault auto-harvest error: {e}", category="vault")

        threading.Thread(target=_worker, daemon=True).start()
