# Pawchive Downloader

<p align="center">
  <img src="assets/icon.png" alt="Pawchive Downloader logo" width="96" height="96" />
</p>

<p align="center">
  <strong>A desktop archiver for Pawchive, cum.st, Telegram and cloud hosts — with a new, clean interface.</strong><br>
  Tauri (Rust) shell · React · Lucide icons · a download engine moving from Python to Rust.
</p>

---

## What this is

This is a fork of [Pawchive Downloader](https://github.com/whyamihere773/Pawchive-Downloader) that throws out the
Qt / QML interface and replaces it with a new desktop app built on **Tauri** and **React**. Underneath, the Python
download engine is being rewritten in **Rust**, one part at a time: the downloads themselves, scanning sites, planning
what to save, the Watchlist and the download archive already run in Rust, and every feature keeps working while the
rest moves over.

<p align="center">
  <img src="assets/screenshots/downloader-dark.png" alt="The Downloader page" width="95%" />
</p>

**What changed from the original:**

- **A completely new interface** (`desktop/`): consistent shadcn-style components, Lucide icons, Geist type, a
  sidebar layout, light and dark themes, and layouts that fit any window size without overflowing.
- **A Rust download engine** (`desktop/src-tauri/src/`): downloads, the queue, site scans, the download planner,
  the Watchlist (its checks, its database and its list), the download archive's lookups and the account favorites
  run in Rust. Each part is checked against the Python code it replaces before it switches over.
- **The rest runs as a background process** (`server/`): what's still Python (Telegram, the cloud hosts, the
  Decompressor and more) runs out of sight; the Rust shell starts it, talks to it over a local, token-protected
  connection, and shuts the whole process tree down with the app.
- **Pawchive and cum.st**, plus Telegram, Bunkr, Erome, nHentai and cloud links (GoFile, MEGA, Google Drive,
  Dropbox). Links from unsupported archive sites are refused with a pointer to Pawchive or cum.st, and Watchlist
  creators from them can be moved to Pawchive in one click.
- **Feels like an app, not a web page**: no browser right-click menu or browser shortcuts; text fields get their own
  Cut / Copy / Paste menu.

## Features

| | |
|---|---|
| **Downloader** | Paste a creator, post or channel link; pick file types, page range, skip words, characters, dates and sizes; choose individual posts before downloading |
| **Queue** | Follow every file or every post, pause / resume / retry / remove a whole post, retry failures, import and export |
| **Watchlist** | Follow creators, check for new posts on startup or on demand, download only what's new |
| **Gallery** | Browse downloads with thumbnails, a full-screen image and video viewer, rename / move / delete (to the Recycle Bin) |
| **Archive** | A record of every downloaded file so nothing is fetched twice — search it, check files on disk, import, export or rebuild it from folders |
| **Link vault** | External links (MEGA, Google Drive, Dropbox…) collected from posts, with their passwords and link health checks |
| **Decompressor** | Find and extract archives in bulk, with a password bank and prompts for locked files |
| **And more** | Activity log, download history, accounts (incl. Telegram), bandwidth limits, proxy |

## Screenshots

<table>
  <tr>
    <td><img src="assets/screenshots/watchlist-dark.png" alt="Watchlist" /></td>
    <td><img src="assets/screenshots/downloader-light.png" alt="Downloader, light theme" /></td>
  </tr>
  <tr>
    <td align="center"><b>Watchlist</b>: follow creators and download only what's new</td>
    <td align="center"><b>Light theme</b></td>
  </tr>
  <tr>
    <td colspan="2" align="center"><img src="assets/screenshots/settings-dark.png" alt="Settings" width="50%" /></td>
  </tr>
  <tr>
    <td colspan="2" align="center"><b>Settings</b></td>
  </tr>
</table>

### Supported sites

- **Creator archives:** Pawchive (`pawchive.pw`), cum.st, Telegram channels and posts
- **Creator services:** Patreon, Pixiv Fanbox, Fantia, SubscribeStar, Gumroad, Boosty, Discord, OnlyFans, Fansly, Afdian, DLsite and more
- **Cloud drives:** MEGA, Google Drive, Dropbox, GoFile
- **Galleries & lockers:** Bunkr, Erome, nHentai, Saint2, Pixeldrain, Catbox, MediaFire, SimpCity
- **Video embeds:** YouTube, Vimeo, Streamable, RedGifs, Twitter/X, Bilibili, SoundCloud, Dailymotion

## Running it

There's no packaged release of the new app yet, so for now it runs from source.

**You need:** Python 3.10+, Node.js 20+, and Rust (on Windows, the MSVC toolchain with WebView2 — already on Windows 10/11).

```bash
git clone https://github.com/shybruh/Pawchive-Downloader
cd Pawchive-Downloader
python -m venv .venv
.venv\Scripts\python -m pip install -r requirements.txt
```

```bash
cd desktop
npm install
npm run tauri dev
```

See [`desktop/README.md`](desktop/README.md) for how the shell, the engine and the UI fit together, and
[`docs/AUDIT.md`](docs/AUDIT.md) for what has moved to Rust so far and what's next.

## Credits

This project stands on the work of others:

- **[Kemono Downloader](https://github.com/Yuvi9587/Kemono-Downloader)** by **Yuvi9587** — the original that started
  it all and the inspiration for everything that came after.
- **[Pawchive Downloader](https://github.com/whyamihere773/Pawchive-Downloader)** by **whyamihere773** and
  contributors — the engine this app runs on, and the project this fork is based on. Its interface is what inspired
  this rebuild.

## License

This fork keeps the original project's custom source-available license: personal, non-commercial use only. See
[LICENSE](LICENSE). It bundles [yt-dlp](https://github.com/yt-dlp/yt-dlp) (public domain).

## Disclaimer
**AI use:** Pawchive Downloader was originally developed with the help of AI coding assistants (Claude).
The new downloader also uses the help of AI assistants for development(Claude and Qwen).
This tool is meant for personal archiving and backup of content you have the right to access. Respect creators'
terms and rights.
