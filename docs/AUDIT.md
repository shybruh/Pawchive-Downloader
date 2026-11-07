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
| Unsupported archive sites | their mirror, file-server and metadata fallbacks, a `Referer` header for them sent to every other host, their accounts and every mention in the interface |
| Qt Quick / QML in the engine | the thumbnail code no longer uses QML image providers; the packaged engine leaves out Qt Quick, QML, PDF, OpenGL and other unused Qt modules (486 MB → 124 MB) |
| Upstream repo files | `SECURITY.md`, `.github/FUNDING.yml` |

Kept on purpose: `ffmpeg_manager` (the video-length filter needs ffprobe), `franchise_resolver`
(plain keyword matching from Known.txt), keyword tagging in the gallery tools.

## 3. Porting the engine to Rust

The goal is no Python at all. Nothing in the engine needs Python: PySide6 / Qt goes away with the
host below, the helper programs (yt-dlp, 7-Zip, ffmpeg) are separate executables, and every library
has a Rust counterpart (`reqwest` + `tokio`, `rusqlite`, `aes` / `ctr` for MEGA, `image` for
thumbnails, the `windows` crate for the DPAPI credential vault). The one open question is Telegram:
the `grammers` crate has to cover QR sign-in, two-step passwords and large media downloads; if it
can't, Telegram alone stays a small Python helper.

### How: the engine host

The UI talks to one engine through `src/lib/engine.ts`. Since step 1 that connection goes to the
**engine host** in the Rust shell (`src-tauri/src/host/`), not to Python:

- objects implemented in Rust (`host/native.rs`, registered in `host::natives()`) are answered there;
- every other request is passed through to the Python engine untouched, and its answers and events
  flow back; `describe` gets the Rust objects merged in, so the UI sees one engine.

Porting a part means writing its object in Rust with the same name, properties, methods and signals,
registering it, and deleting the Python object. The UI doesn't change.

### Shared data while both engines run

A ported object owns its data files from then on (same files, same format, so updates lose nothing).
Until everything that reads those files is ported too, the Python code that still reads them must
re-read a file when it changes on disk instead of keeping its own copy, or the two will overwrite
each other. Port a part together with everything that writes its data.

### Order

| Step | Part | Status |
|---|---|---|
| 1 | Engine host in the shell (pass-through, `shell` object) | done |
| 2 | Media server (thumbnails, images and files for the Gallery and previews): `src-tauri/src/media/`, same thumbnail cache | done |
| 3 | Accounts and the credential vault: `src-tauri/src/accounts/` (same encrypted file; sign-in, Test, browser import). Python keeps a small reader (`core/auth_manager.py`) that re-reads the vault when it changes | done |
| 4 | Scheduler | removed: the scheduler was dropped from the app instead of ported |
| 5 | Link vault (`linkVault`, `core/link_vault_manager.py`) and cloud hosts (`services/cloud_downloader.py`) | next |
| 6 | Downloader core: link parsing, site clients, filters and naming, downloader and queue (`core/parser.py`, `api_client.py`, `filter_engine.py`, `folder_naming.py`, `downloader.py`, `bridge/queue_model.py`) — the largest part | |
| 7 | Archive, watchlist, Known series (SQLite and JSON) | |
| 8 | Gallery tools, Decompressor (still runs 7-Zip), Bunkr / Erome / nHentai, embeds (still run yt-dlp) | |
| 9 | Telegram, then remove the Python engine and PyInstaller from the build | |

Each step is checked against the running app before its Python code is deleted.

## 4. Still to fix regardless of the port

- `bridge/app_bridge.py` is still one large object (~7,100 lines). It's being split by feature:
  each feature moves into its own engine object with its own name in `server/sidecar.py`, and the
  UI talks to that object directly. Each object is one unit to port to Rust.

  | Object | Covers | Status |
  |---|---|---|
  | `accounts` (`bridge/accounts_bridge.py`) | site sign-in, cookies, session test | done |
  | `scheduler` | removed with the scheduler | — |
  | `linkVault` (`bridge/link_vault_bridge.py`) | saved cloud links: collect, check, edit, export (downloading them stays with downloads) | done |
  | `favorites` | account favorites browse / download | next |
  | `watchlist` | watchlist slots and the background check | |
  | `postSelection` | "Select posts…" fetch and the picker's results | |
  | `downloads` | start / stop / pause, queue import and export, run results | |
  | `settings` | the option properties (what remains in `appBridge`) | last |

  The bridge members nothing used (~2,300 lines: Qt-interface leftovers, and engine features the
  new UI never exposed such as batch rename, auto-sort, broken-file scan and the video-length
  filter) were deleted first.
- Browser cookie import only works with Firefox: Chrome, Edge and Brave encrypt cookies with
  app-bound keys. Signing in inside the app (`src-tauri/src/site_login.rs`) is the main way to log in.
- The unsupported archive sites' domains appear only where they must: recognising their links (to
  refuse them with a pointer to Pawchive / cum.st), migrating an old saved login, and mapping old
  image links in post text to Pawchive.
