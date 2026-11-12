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
| AI character recognition | `ai_semantic_matcher`, `ai_reasoner`, `model_manager`, `hardware_detector`, onnxruntime / tokenizers / GGUF models (`requirements-ai.txt`). |
| Privacy mode, PIN lock, safe mode | properties, slots and settings in `app_bridge` |
| Storage pools (overflow to other drives) | `storage_pool_manager` and its hooks in the downloader and watchlist |
| Old self-updater | `update_service`, `update_installer`, `updater_bridge` (the app has its own signed updater) |
| Desktop completion reports | `report_generator` |
| Gallery archive tools | `gallery_archive_bridge` |
| Planner options the window no longer offered | the video-length filter (`video_probe`), manga mode (oldest first) and tag folders, in the Python and Rust planners |
| Known matching's unused parts | the character database (`data/master_characters.json` never shipped, so it always loaded empty), the recognition mode that chose between it and Known.txt, AniList online lookups (never switched on), the AI hooks, and the download archive's creator character profile (only a cache key) |
| Known series | the Known list and its page (`known_manager`, `franchise_resolver`, `known_model`), sorting downloads into series and character folders (the Python and Rust planners, `src-tauri/src/known/`), its names in post_info.txt tags and the gallery's character tags, the "possible character names" log line; Known.txt and the learned franchise cache are left where they are |
| Video fingerprinting, media compression | `video_fingerprinter`, `media_compressor`, the Decompressor's "compress after extracting" |
| Python workarounds | `memory_collector`, `hang_detector` |
| The Qt / QML interface | `qml/`, `main.py`, `updater.py`, the old build script and PyInstaller specs |
| Translations | `locales/` (15 languages + tutorials), `translation_manager`, the Language setting; the interface is English-only |
| Unsupported archive sites | their mirror, file-server and metadata fallbacks, a `Referer` header for them sent to every other host, their accounts and every mention in the interface |
| Qt Quick / QML in the engine | the thumbnail code no longer uses QML image providers; the packaged engine leaves out Qt Quick, QML, PDF, OpenGL and other unused Qt modules (486 MB → 124 MB) |
| Upstream repo files | `SECURITY.md`, `.github/FUNDING.yml` |

Kept on purpose: `ffmpeg_manager` (the Decompressor uses ffmpeg), keyword tagging in the gallery tools.

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
| 5 | Link vault (`linkVault`, `core/link_vault_manager.py`) and cloud hosts (`services/cloud_downloader.py`): after step 6, since collecting links needs the site client and cloud downloads run in the queue | in progress: MEGA, Dropbox and GoFile downloads run in Rust (`src-tauri/src/downloader/cloud.rs`, see step 6); still Python: Google Drive, the Link Vault itself |
| 6 | Downloader core: link parsing, site clients, filters and naming, downloader and queue (`core/parser.py`, `api_client.py`, `filter_engine.py`, `folder_naming.py`, `downloader.py`, `bridge/queue_model.py`) — the largest part | in progress: file transfers done (`src-tauri/src/transfer/`); link parsing, filters, naming, the site client, link/password extraction, file ordering, the download planner and the download engine (queue, worker pool, per-file download) ported (`src-tauri/src/downloader/`, checked against 2,767 Python answers, 36 planner cases, engine tests on a local test server and live downloads, `tools/golden_*.py`). **Downloads now run in Rust**: `services/rust_queue.py` hands the queue to the Rust engine over a local socket (`downloader/service.rs`) while the bridge and the window's models stay in Python (`PAWCHIVE_PYTHON_DOWNLOADER=1` runs the old loop). **Scans and planning run in Rust too**: the app's site requests (`services/rust_sites.py`) and the download planner (`RustDownloader.build_tasks_from_posts`) are calls to the same service, with the same log lines; the download archive is looked up in Rust as well (`PAWCHIVE_PYTHON_SITES=1` / `PAWCHIVE_PYTHON_PLANNER=1` use the Python ones). **The window's queue view runs in Rust**: the file list, its filters, the counts and the per-post groups (`downloader/queue_view.rs`, checked against 28 cases of the Python model) are served to the window by the host (`host/queue.rs`); the bridge's queue model (`services/rust_queue_view.py`) sends its files over. **The queue's post and file actions run in Rust**: pausing, resuming, cancelling and removing a post, pausing and resuming a file are done on the engine's own copy of the queue (`downloader/service.rs` `queue_window`, which Python now keeps up to date between runs too), the app following (its paused posts, the removed files, starting a run when files are waiting); so are the retries (Retry failed, a post's failed files, one file: `queue.rs` `retry_failed_cells` / `retry_quiet`), cloud links still retried by Python; the failed files saved on disk for Retry Failed are put back in Rust's queue when it has none (Retry Failed, or one of them: `journal.rs` `saved_failed`, `queue.rs` `adopt`; the app's list takes them in), Python still writing that file. So does Clear failed (`queue.rs` `clear_failed`, the app taking the same files out of its lists and of the failed files saved for Retry Failed). **Rust's list is becoming the queue**: new files are added by Rust's queue (`queue.rs` `add`: files already queued left out, save paths kept apart; matched the Python way in 11 cases), Python's list taking its answer, and a run's start no longer overwrites Rust's file states with Python's. The crash journal is written by Rust from its own list (`downloader/journal.rs`: the same SQLite file and rows, only what changed, a save from before a discard dropped; matched the Python journal's rows, `tools/golden_journal.py`; a crash mid-download was resumed from it); Rust reads it back too (`journal.rs` `has_unfinished` / `summary` / `load`: the check for an unfinished download at start, the recovery prompt's summary, the queue Resume restores; matched the Python journal's answers); Python still checks restored files against the disk (after translating paths saved on the other system) and fills the queue. Queue import merges through Rust's queue. **Python's list is a copy of Rust's**: setting it sets Rust's (`queue_set`: files already there keep their states, those being downloaded stay) and it becomes what Rust then holds, in Rust's order; it is no longer watched and sent over during and between runs (only when the engine reconnects, or after it was set while the engine couldn't be asked). Still Python's: the export / import dialogs, the failed files saved on disk for Retry Failed, cancelling everything (scans, cloud, Telegram, yt-dlp too), and the bridge's own two window lists (the queue models), which still decide what goes in a new download's list. **A link's download is scanned and planned in one Rust call** (`downloader/scan.rs` `link_scan`, for the archive sites: the creator's name, the posts, one post or the DMs, the DMs with the posts when wanted, favorite mode's filter, the comments for the character filter, the plan; the creator's folder asked from the app, the watchlist's when it has one); the bridge keeps the status line, the history, the watchlist entry, the Link Vault and putting the files in the queue (the old way when the engine can't be asked). **The post picker runs in Rust too** for the archive sites (`downloader/selection.rs`: its scan, the preview cards, keeping the scanned posts, then the posts and files ticked and their plan; cards and picking matched the Python way, `core/post_selection.py`, `tools/golden_selection.py`); the bridge keeps the dialog's signals and putting the files in the queue. **The album sites are read in Rust** (`downloader/albums.rs`: Bunkr albums, file pages, download links and signed links, Erome albums, nHentai galleries; matched the Python clients on 25 made-up sites' pages, `tools/golden_albums.py`; not yet tried on live links); the bridge names their folders and queues them, and reads a Bunkr album's title alone (the name shown for a pasted link) the Python way (`services/rust_albums.py`, `PAWCHIVE_PYTHON_ALBUMS=1` for the Python clients). **MEGA, Dropbox and GoFile links download in Rust** (`downloader/cloud.rs`: MEGA files and folders decrypted on the way and continued from a `.part`, Dropbox files and zipped folders, GoFile through its official API with a Premium token only; the same log lines, filters, pause, cancel and speed limit; matched the Python downloaders on 25 made-up sites with really encrypted MEGA files, `tools/golden_cloud.py`, and a real MEGA file's decryption checked against Python's); the bridge still queues them and names their folders (`services/rust_cloud.py`, `PAWCHIVE_PYTHON_CLOUD=1` for the Python ones). Google Drive is still downloaded by Python (`gdown`). Next: Google Drive |
| 7 | Archive and watchlist (SQLite) | in progress: the watchlist's new-post check, its database, the window's watchlist and the creators with their operations (adding, favorites, removing, the "downloaded up to" point, marking updates done or ignored, auto-check, download folders; `watchlist/entries.rs`, checked against 46 operations of the Python manager, `tools/golden_watchlist_entries.py`) and the Watchlist tab's simple buttons (remove, auto-check, ignore a post: answered by `src-tauri/src/bridge/`, Python's copy and the window's list follow); the check for new posts runs in Rust too (`watchlist/check.rs`: the round with its resuming, site lists and spot checks, each creator's name, new posts and the look for files already downloaded, one creator's check from its button; matched the Python checker on real creators) and the work before a watchlist download (its new posts or the ones picked, its folder, its plan; marked up to date when everything is downloaded already: `watchlist/check.rs` `download`, matched the Python way on a real creator) run in Rust; the account favorites run in Rust too (the signed-in accounts' favorite creators fetched, added to the watchlist or marked as favorites, the favorites window's cards and their new posts; for "Get all new", the creators to download: `watchlist/favorites.rs`, matched the bridge's Python way with a real account) (`src-tauri/src/watchlist/`; Python keeps a synced copy of every creator for the code still reading them, and does an operation itself when the engine can't be asked, `PAWCHIVE_PYTHON_WATCHLIST=1` turns the Rust one off); the download archive's lookups and records (what downloading and planning do for every file) run in Rust (`src-tauri/src/archive/`, the same database file; Python closing it for a repair holds the Rust side). Still Python: starting the watchlist's checks and downloads (the signals to the window, switched-off sites), putting its downloads in the queue and their links in the Link Vault, the Archive tab, archive maintenance and repair |
| 8 | Gallery tools, Decompressor (still runs 7-Zip), Bunkr / Erome / nHentai, embeds (still run yt-dlp) | |
| 9 | Telegram, then remove the Python engine and PyInstaller from the build | |

Each step is checked against the running app before its Python code is deleted. Pure logic (link parsing, filters, naming) is ported with a reference set: `tools/golden_*.py` saves the Python version's answers to `src-tauri/tests/fixtures/`, and `cargo test` checks the Rust version gives the same for every case. Python keeps using its own copy until the Rust downloader takes over.

## 4. Still to fix regardless of the port

- `bridge/app_bridge.py` is still one large object (~7,100 lines). It's being split by feature:
  each feature moves into its own engine object with its own name in `server/sidecar.py`, and the
  UI talks to that object directly. Each object is one unit to port to Rust.

  | Object | Covers | Status |
  |---|---|---|
  | `accounts` (`bridge/accounts_bridge.py`) | site sign-in, cookies, session test | done |
  | `scheduler` | removed with the scheduler | — |
  | `linkVault` (`bridge/link_vault_bridge.py`) | saved cloud links: collect, check, edit, export (downloading them stays with downloads) | done |
  | Gallery file explorer (in `appBridge`) | folder listing and sizes, breadcrumbs, drives, move, rename, duplicates, path safety, open / reveal | in Rust (`src-tauri/src/bridge/gallery.rs`, answered by the host for `appBridge`); Recycle Bin delete and the bookmarks still in Python |
  | `favorites` | account favorites browse / download | in Rust (`src-tauri/src/watchlist/favorites.rs`); each creator's download goes the watchlist download's way |
  | `watchlist` | watchlist slots and the background check | the check for new posts, the database (watchlist.db) and the window's list (`appBridge.watchlistModel`: filters, order, counts, on-disk check) run in Rust (`src-tauri/src/watchlist/`, `host/watchlist.rs`); the entries and the bridge's watchlist actions are still Python. Favorites wait for it: they are watchlist entries |
  | `postSelection` | "Select posts…" fetch and the picker's results | |
  | `downloads` | start / stop / pause, queue import and export, run results | |
  | `settings` | the option properties (what remains in `appBridge`) | last |

  Parts of `appBridge` can move to Rust before the whole object does: the host answers the
  methods a Rust part handles (`host/native.rs` `Overlay`) and passes the rest to Python.

  The bridge members nothing used (~2,300 lines: Qt-interface leftovers, and engine features the
  new UI never exposed such as batch rename, auto-sort, broken-file scan and the video-length
  filter) were deleted first.
- Browser cookie import only works with Firefox: Chrome, Edge and Brave encrypt cookies with
  app-bound keys. Signing in inside the app (`src-tauri/src/site_login.rs`) is the main way to log in.
- The unsupported archive sites' domains appear only where they must: recognising their links (to
  refuse them with a pointer to Pawchive / cum.st), migrating an old saved login, and mapping old
  image links in post text to Pawchive.
