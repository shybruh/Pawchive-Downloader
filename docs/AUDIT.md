# Feature audit and Rust port plan

What the old Qt interface could do that the new one can't yet, what's worth keeping, and what to
cut from the Python engine before porting it to Rust.

The engine is about **53,000 lines of Python** (`core/` ~25k, `bridge/` ~18k, `services/` ~10k).
A lean Rust engine needs maybe a third of that.

## 1. Old features missing from the new interface

### Worth adding to the new interface

| Feature | Engine API (still there) | Why it matters |
|---|---|---|
| **Telegram downloads** | `telegramScopeRequested`, `openTelegramPostSelection`, `queueTelegramFiles`, phone / 2FA / bot-token login | Telegram channel links need a scope step and full sign-in; only QR sign-in is in the new UI. Telegram downloads are probably broken until this is done. |
| **Favorites browser** | `fetchFavoriteArtists`, `downloadFavoriteArtist`, `downloadAllFavoriteArtists` | Favorites mode can start a download, but you can't browse or pick favorite creators |
| **Download extracted cloud links** | `harvestedLinks`, `startCloudDownloads`, `exportAllLinks` | "Links only" mode collects MEGA / Drive / Dropbox links; there's no screen to download them (the Link vault keeps them) |
| **Watchlist details** | per-creator folders, `ignoreWatchlistPost`, `unignoreAllWatchlistPosts`, `setWatchlistLastDownloadDate` | Choosing where each creator saves, skipping posts you don't want |
| **Load many links from a file** | `batchLoadUrls` | Queue a list of links in one go |
| **Filename template preview** | `previewCustomFilename`, `detectNextIndex` | Shows what a custom template produces before downloading |
| **Gallery basics** | `createFolder`, `undoLast`, `setRating`, `setFavorite`, `startSearch` | New folder, undo a move / rename, ratings, search across folders |
| **Batch rename, duplicate finder** | `previewBatchRenameAsync` / `executeBatchRenameAsync`, `scanDuplicatesAsync` | Useful library cleanup, but secondary |

Added in this round: crash recovery (resume interrupted downloads) and the "when the queue
finishes" countdown. Before this, the engine waited for a countdown the new UI never showed,
so "shut down / sleep when finished" didn't happen.

### Not worth porting (cut instead)

| Feature | Engine code | Why cut it |
|---|---|---|
| **AI character recognition** | `ai_semantic_matcher`, `ai_reasoner`, `model_manager`, `hardware_detector`, most of `known_manager` (2k lines) and `franchise_resolver` | Pulls in onnxruntime, tokenizers and multi-GB GGUF models to guess character names. A plain keyword list does most of the job. |
| **Privacy mode / PIN lock / safe mode** | spread through `app_bridge` | The OS already locks the screen; it adds state to almost every screen |
| **Storage pools** (overflow to other drives) | `storage_pool_manager` | Niche; one download folder per creator covers it |
| **Self-updater** | `update_service`, `update_installer` (~1.5k lines) | Replaced by the app's own signed updater; it also still points at the original project |
| **Desktop reports** | `report_generator` | Rarely useful; the History screen covers it |
| **Gallery archive tools** | `gallery_archive_bridge` | Previewing and creating archives inside the gallery duplicates the Decompressor |
| **AI tagging, video fingerprinting, media compression** | `gallery_tools_bridge` (tagging), `video_fingerprinter`, `media_compressor`, `ffmpeg_manager` | Heavy (ffmpeg, models) for edge cases; can come back later as optional tools |
| **Memory collector, hang detector** | `memory_collector`, `hang_detector` | Workarounds for Python and Qt; not needed in Rust |
| **Custom extension lists** | `toggleExactExtension`, `savedCustomExtensions` | Fold into the file-type picker |

## 2. What the Rust engine should contain

Port in this order. Each step can replace its Python counterpart while the rest keeps running in
the sidecar.

1. **Downloader + queue** — `core/downloader.py`, `services/multipart_downloader.py`,
   `bridge/queue_model.py`, `core/queue_journal.py`, `core/recovery_manager.py`.
   Parallel, resumable downloads (`.part` files), per-file and per-post pause, retries, bandwidth
   limit, crash journal. Crates: `reqwest`, `tokio`, `sqlx` or `rusqlite`.
2. **Site clients** — `core/api_client.py`, `core/parser.py`, `core/providers.py`,
   `core/auth_manager.py`, `services/cookie_importer.py`. Pawchive and cum.st API, link parsing,
   accounts and cookies (Windows DPAPI via `windows` crate).
3. **Filters and naming** — the useful half of `core/filter_engine.py` and `core/folder_naming.py`
   (file types, skip words, dates, sizes, filename templates).
4. **Archive** — `core/archive_manager.py`, `core/archive_index.py` (SQLite; mostly SQL, ports easily).
5. **Watchlist + scheduler** — `core/watchlist_manager.py`, `core/watchlist_checker.py`,
   `core/task_scheduler.py`.
6. **Link vault + cloud hosts** — `core/link_vault_manager.py`, `services/link_extractor.py`,
   `services/cloud_downloader.py` (MEGA needs AES; `aes` / `ctr` crates).
7. **Decompressor** — `services/bulk_decompressor.py`; keep calling 7-Zip as an external program.
8. **Other hosts** — Bunkr, Erome, nHentai clients; embeds keep using the `yt-dlp` program.
9. **Telegram** — the hardest (Telethon). Options: the `grammers` crate, or keep a small Python
   helper just for Telegram.

The engine API the UI uses (`src/lib/engine.ts`) doesn't need to change: a Rust engine can expose
the same objects, properties and methods over the same WebSocket, or the shell can call it
directly as Tauri commands.

## 3. Things to fix regardless of the port

- `bridge/app_bridge.py` is a 10,500-line object holding every feature. Split by feature when porting.
- Requests send `X-Contact` headers pointing at the original project's GitHub
  (`core/downloader.py`, `_CONTACT_HEADERS`); point them at this fork or remove them.
- Settings, data and logs live next to the engine executable; an installed app should keep them in
  the user's app-data folder so reinstalls and updates never touch them.
- Kemono / Coomer code paths are switched off but still present (fallbacks to their file servers in
  `core/downloader.py`); delete them in the Rust version.
