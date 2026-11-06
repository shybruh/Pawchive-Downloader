# Feature audit and Rust port plan

What changed from the original Qt app, what was cut, and how to port the remaining Python engine
to Rust.

## 1. Brought over from the old interface

| Feature | Where |
|---|---|
| Telegram downloads: scope step (how many posts, media types, size cap), choose posts or download directly | Downloader → Telegram link |
| Telegram sign-in: QR code, phone number + code + 2FA, bot token, custom API ID / hash | Settings → Accounts, or when a Telegram link needs it |
| Account favorites: browse favorited creators, get new posts, download everything, choose posts | Downloader "…" menu, Watchlist "…" menu, Favorites mode |
| Collected cloud links (MEGA, Google Drive, Dropbox, GoFile) from "Links only" runs: review, download, export | Downloader banner and "…" menu; Link vault → Download |
| Watchlist details: per-creator folder (optionally moving files), "downloaded up to" date, skip individual new posts, show skipped again | Watchlist → creator → Manage… |
| Queue a list of links, pasted or from a file | Downloader "…" menu |
| Gallery: new folder, undo (moves, renames, new folders), search inside subfolders, duplicate finder | Gallery toolbar and "…" menu |
| Crash recovery and the "when the queue finishes" countdown | App-wide |

Not brought over (low value): batch rename, filename-template preview, gallery ratings / favorites
marks, AI tagging, storage breakdown.

## 2. Cut from the engine

| Removed | Was |
|---|---|
| AI character recognition | `ai_semantic_matcher`, `ai_reasoner`, `model_manager`, `hardware_detector`, onnxruntime / tokenizers / GGUF models (`requirements-ai.txt`). Known series now matches with Known.txt and the character database only. |
| Privacy mode, PIN lock, safe mode | properties, slots and settings in `app_bridge` |
| Storage pools (overflow to other drives) | `storage_pool_manager` and its hooks in the downloader and watchlist |
| Old self-updater | `update_service`, `update_installer`, `updater_bridge` (the app has its own signed updater) |
| Desktop completion reports | `report_generator` |
| Gallery archive tools | `gallery_archive_bridge` |
| Video fingerprinting, media compression | `video_fingerprinter`, `media_compressor`, the Decompressor's "compress after extracting" |
| Python workarounds | `memory_collector`, `hang_detector` |
| The Qt / QML interface | `qml/`, `main.py`, `updater.py`, the old build script and PyInstaller specs |
| Translations | `locales/` (15 languages + tutorials), `translation_manager`, the Language setting; the interface is English-only |
| Kemono / Coomer fallbacks | mirror and file-server fallbacks, metadata fallbacks, a `Referer: kemono.cr` header sent to every other host |
| Qt Quick / QML in the engine | the thumbnail code no longer uses QML image providers; the packaged engine leaves out Qt Quick, QML, PDF, OpenGL and other unused Qt modules (486 MB → 124 MB) |
| Upstream repo files | `SECURITY.md`, `.github/FUNDING.yml` |

Kept on purpose: `ffmpeg_manager` (the video-length filter needs ffprobe), `franchise_resolver`
(plain keyword matching from Known.txt), keyword tagging in the gallery tools.

## 3. What the Rust engine should contain

Port in this order; each step can replace its Python counterpart while the rest keeps running in
the sidecar.

1. **Downloader + queue** — `core/downloader.py`, `services/multipart_downloader.py`,
   `bridge/queue_model.py`, `core/queue_journal.py`, `core/recovery_manager.py`.
   Parallel, resumable downloads (`.part` files), per-file and per-post pause, retries, bandwidth
   limit, crash journal. Crates: `reqwest`, `tokio`, `rusqlite`.
2. **Site clients** — `core/api_client.py`, `core/parser.py`, `core/providers.py`,
   `core/auth_manager.py`, `services/cookie_importer.py`.
3. **Filters and naming** — `core/filter_engine.py`, `core/folder_naming.py`.
4. **Archive** — `core/archive_manager.py`, `core/archive_index.py` (SQLite).
5. **Watchlist + scheduler** — `core/watchlist_manager.py`, `core/watchlist_checker.py`,
   `core/task_scheduler.py`.
6. **Link vault + cloud hosts** — `core/link_vault_manager.py`, `services/link_extractor.py`,
   `services/cloud_downloader.py` (MEGA needs AES; `aes` / `ctr` crates).
7. **Decompressor** — `services/bulk_decompressor.py`; keep calling 7-Zip as an external program.
8. **Other hosts** — Bunkr, Erome, nHentai clients; embeds keep using the `yt-dlp` program.
9. **Telegram** — the hardest (Telethon). Options: the `grammers` crate, or a small Python helper
   just for Telegram.

The engine API the UI uses (`src/lib/engine.ts`) doesn't need to change: a Rust engine can expose
the same objects, properties and methods over the same WebSocket, or the shell can call it
directly as Tauri commands.

## 4. Still to fix regardless of the port

- `bridge/app_bridge.py` is still one large object (~7,500 lines). It's being split by feature:
  each feature moves into its own engine object with its own name in `server/sidecar.py`, and the
  UI talks to that object directly. Each object is one unit to port to Rust.

  | Object | Covers | Status |
  |---|---|---|
  | `accounts` (`bridge/accounts_bridge.py`) | site sign-in, cookies, session test | done |
  | `scheduler` (`bridge/scheduler_bridge.py`) | scheduler settings and the schedule list (running a schedule stays with downloads) | done |
  | `linkVault` | collected cloud links, cloud downloads | next |
  | `favorites` | account favorites browse / download | |
  | `watchlist` | watchlist slots and the background check | |
  | `postSelection` | "Select posts…" fetch and the picker's results | |
  | `downloads` | start / stop / pause, queue import and export, run results | |
  | `settings` | the option properties (what remains in `appBridge`) | last |

  The bridge members nothing used (~2,300 lines: Qt-interface leftovers, and engine features the
  new UI never exposed such as batch rename, auto-sort, broken-file scan and the video-length
  filter) were deleted first.
- Browser cookie import only works with Firefox: Chrome, Edge and Brave encrypt cookies with
  app-bound keys. Signing in inside the app (`src-tauri/src/site_login.rs`) is the main way to log in.
- Kemono / Coomer names remain only where they're harmless: link recognition (so their links are
  refused with a clear message), the "kemono" credential slot that stores the general cookie, and
  internal class names. Drop them in the Rust version.
