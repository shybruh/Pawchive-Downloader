# Pawchive Downloader — desktop app (Tauri + React)

> Based on [Pawchive Downloader](https://github.com/whyamihere773/Pawchive-Downloader). This fork replaces
> the Qt / QML interface with a Tauri + React one, keeps the original Python engine running in the
> background. See the root `LICENSE`.

A small Rust shell (Tauri) starts the existing Python engine in the background, and the React UI
talks to it over a local WebSocket.

```
┌──────────────── Tauri shell (Rust, src-tauri/) ────────────────┐
│ starts  python -m server.sidecar --port 0 --token …            │
│ (in a job object, so its whole process tree ends with the app) │
│ engine host (src/host/): answers the objects ported to Rust,   │
│ passes everything else through to the Python engine            │
└──────┬──────────────────────────────────────────────┬──────────┘
       │ ws://127.0.0.1:<host port>/?token=…           │ ws (same protocol)
┌──────┴───────────────────────┐      ┌───────────────┴──────────────────┐
│ React UI (src/)              │      │ Python engine (server/sidecar.py)│
│ shadcn-style components,     │      │ same bridges as the old QML app, │
│ lucide icons, Tailwind v4    │ ←──  │ reflected automatically          │
└──────────────────────────────┘ HTTP └──────────────────────────────────┘
          thumbnails / images / video   (src-tauri/src/media/, Rust)
```

The engine is being ported to Rust one object at a time through the host; see `docs/AUDIT.md`.

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

Handy for UI work: give the host (and the engine behind it) a fixed port and token, and point a
second Vite at the host.

```bash
PAWCHIVE_HOST_PORT=47322 PAWCHIVE_HOST_TOKEN=devtoken PAWCHIVE_ENGINE_PORT=47321 PAWCHIVE_ENGINE_TOKEN=enginetoken PAWCHIVE_TRANSFER_PORT=47323 PAWCHIVE_TRANSFER_TOKEN=xfertoken npm run tauri dev
VITE_ENGINE_PORT=47322 VITE_ENGINE_TOKEN=devtoken npx vite --port 1430
```

## Releases and updates

`.github/workflows/release.yml` builds and publishes a release whenever a version tag is pushed:

```bash
git tag v2.0.1
git push origin v2.0.1
```

It packages the engine with PyInstaller, builds the app, and uploads an installer, a portable zip
and `latest.json` to the GitHub release. The installed app checks `latest.json` a few seconds after
it starts (Settings → Updates also checks on demand) and can update and restart itself.

Updates are signed. The workflow needs the private signing key as the repository secret
`TAURI_SIGNING_PRIVATE_KEY`; the matching public key is in `src-tauri/tauri.conf.json`. Keep a
backup of the private key: without it, already-installed copies can't verify new releases.

For a local build, run `python build_desktop.py` from the repository root (see the script).

## Screens

| Screen | |
|---|---|
| Downloader | link + source detection, folder and pages, file types, filters & options, post picker, session, live log |
| Queue | by post (pause / resume / retry / remove a post) or by file, status filters, import / export |
| Watchlist | search, filters, check now / on startup, download new posts, favorites, move unsupported-site creators to Pawchive |
| Activity log | live, searchable, filter by level, export |
| Gallery | places & drives, breadcrumbs, grid / list, thumbnails, multi-select, rename / move / delete (Recycle Bin) with undo, new folder, search inside subfolders, duplicate finder, image viewer and video player |
| History, Known series | ✓ |
| Archive | stats, search & filters, creators → posts → files, check files on disk, forget missing, import / export, rebuild from a folder |
| Link vault | creators → posts → links with health and passwords, collect links from a creator, check links, export, send passwords to the Decompressor |
| Decompressor | scan folders, grouped selection, extract with live progress, password prompts and bank, options |
| Settings | downloads, files & naming, network, accounts (incl. Telegram QR), appearance, updates, about |

The webview's own right-click menu and browser shortcuts are switched off (`src/components/app-guards.tsx`);
text fields get a small Cut / Copy / Paste / Select all menu instead.

## Not done yet

- Gallery extras from the old app: batch rename, ratings.
- Moving engine modules to Rust (downloader and queue first).
