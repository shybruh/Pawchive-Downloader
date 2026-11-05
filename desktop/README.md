# Pawchive Downloader — desktop app (Tauri + React)

> Based on [Pawchive Downloader](https://github.com/whyamihere773/Pawchive-Downloader). This fork replaces
> the Qt / QML interface with a Tauri + React one, keeps the original Python engine running in the
> background, and drops Kemono and Coomer support. See the root `LICENSE`.

A small Rust shell (Tauri) starts the existing Python engine in the background, and the React UI
talks to it over a local WebSocket.

```
┌──────────── Tauri shell (Rust, src-tauri/) ─────────────┐
│ starts  python -m server.sidecar --port 0 --token …     │
│ hands {port, token} to the UI; the engine runs in a job │
│ object, so its whole process tree ends with the app     │
└───────────────┬─────────────────────────────────────────┘
                │ ws://127.0.0.1:<port>/?token=…   (JSON RPC + live events)
┌───────────────┴──────────────┐      ┌──────────────────────────────────┐
│ React UI (src/)              │ ←──→ │ Python engine (server/sidecar.py)│
│ shadcn-style components,     │      │ same bridges as the old QML app, │
│ lucide icons, Tailwind v4    │ ←──  │ reflected automatically          │
└──────────────────────────────┘ HTTP └──────────────────────────────────┘
          thumbnails / images / video   (server/media.py, same token)
```

The sidecar exposes every Qt property, slot and signal of the engine's bridge objects
(`appBridge`, `decompressorBridge`, `telegramBridge`, `updaterBridge`, the queue / log / watchlist
models, …) without hand-written wrappers. See the protocol notes at the top of `server/sidecar.py`
and the client in `src/lib/engine.ts`:

| Hook / function | What it does |
|---|---|
| `useProp(obj, prop)` | live value of an engine property |
| `useSetting(obj, prop)` | live value + setter (writes back; the engine saves settings itself) |
| `useModel(obj)` | rows of a Qt list model, re-fetched when it changes |
| `useSignal(obj, name, fn)` | engine signal handler |
| `call(obj, method, …args)` | call a slot |
| `callAsync(obj, method, …args)` | slots named `…Async` that answer through `asyncResultReady` |
| `mediaUrl(kind, path)` | thumbnail / image / file URL from the media server |

Slow or very large properties (ffmpeg probes, full JSON snapshots) are listed in `LAZY_PROPS` in
the sidecar: they're never read up front or pushed, and big `…Json` values are pushed at most twice
a second.

## Run it (development)

Prerequisites: Node 20+, Rust (MSVC toolchain on Windows), and the Python venv at the repo root:

```bash
python -m venv .venv
.venv/Scripts/python -m pip install -r requirements.txt
```

Then:

```bash
cd desktop
npm install
npm run tauri dev
```

The shell finds the engine in this order: `PAWCHIVE_ENGINE` (a built engine executable),
`pawchive-engine(.exe)` next to the app, then `PAWCHIVE_PYTHON` or `../.venv` running
`-m server.sidecar`. Engine output goes to `engine.log` in the app's log folder.

### Drive the same engine from a browser

Handy for UI work: give the engine a fixed port and token, and point a second Vite at it.

```bash
PAWCHIVE_ENGINE_PORT=47321 PAWCHIVE_ENGINE_TOKEN=devtoken npm run tauri dev
VITE_ENGINE_PORT=47321 VITE_ENGINE_TOKEN=devtoken npx vite --port 1430
```

## Screens

| Screen | |
|---|---|
| Downloader | link + source detection, folder and pages, file types, filters & options, post picker, session, live log |
| Queue | by post (pause / resume / retry / remove a post) or by file, status filters, import / export |
| Watchlist | search, filters, check now / on startup, download new posts, favorites, Kemono → Pawchive switch |
| Activity log | live, searchable, filter by level, export |
| Gallery | places & drives, breadcrumbs, grid / list, thumbnails, multi-select, rename / move / delete (Recycle Bin), viewer for images and video |
| History, Known series | ✓ |
| Archive | stats, search & filters, creators → posts → files, check files on disk, forget missing, import / export, rebuild from a folder |
| Link vault | creators → posts → links with health and passwords, collect links from a creator, check links, export, send passwords to the Decompressor |
| Decompressor | scan folders, grouped selection, extract with live progress, password prompts and bank, options |
| Scheduler | watchlist checks and creator downloads on an interval or daily, night hours, pace |
| Settings | downloads, files & naming, network, accounts (incl. Telegram QR), appearance, updates, about |

The webview's own right-click menu and browser shortcuts are switched off (`src/components/app-guards.tsx`);
text fields get a small Cut / Copy / Paste / Select all menu instead.

## Not done yet

- Release packaging of the engine (`pawchive-engine.exe` via PyInstaller, shipped as a Tauri
  resource). `npm run tauri build` currently builds the shell only.
- Gallery extras from the old app: batch rename, cleaner, duplicate finder, media compression.
- Telegram phone-code / 2FA / bot-token sign-in (QR sign-in is wired).
- The updater still checks the original repository's releases.
- Moving engine modules to Rust (downloader and queue first).
