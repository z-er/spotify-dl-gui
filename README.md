<p align="center">
  <img src="assets/logo.png" alt="spotify-dl-gui logo" width="128"/>
</p>

# spotify-dl GUI

A modern, dark-themed desktop GUI wrapper for [spotify-dl](https://github.com/GuillemCastro/spotify-dl), built with Python + PySide6.
Easily download your Spotify playlists, albums, and tracks with one click, organize your library automatically, and run in the background with Sentry mode.

> [!CAUTION]
> This app is best used with my own version of [spotify-dl](https://github.com/z-er/spotify-dl), which has been updated to include more functionality.
> **I've been made aware the app is flagged by Windows Security- this is a false-positive, a result of using python packaging. You can see the code!**

Current version: **v0.9.5** as of 22/09/2025

---

## Getting Started

### Prerequisites
- Python 3.10+ (tested with 3.11)
- [spotify-dl](https://github.com/GuillemCastro/spotify-dl) installed (`cargo install spotify-dl` or [use provided binary](https://github.com/z-er/spotify-dl))
- Spotify Premium account (required by spotify-dl)

### Install
> [!IMPORTANT]
> Grab the latest release [here!](https://github.com/z-er/spotify-dl-gui/releases)

## Disclaimer

This tool is a community-built GUI for [spotify-dl](https://github.com/GuillemCastro/spotify-dl).
Use responsibly — downloading Spotify content may violate Spotify's Terms of Service and/or local copyright laws.
You are responsible for how you use this software.

---

## Features

- Queue Management
  - Add multiple Spotify links (tracks, albums, playlists).
  - Reorder, remove, or clear queue items.
  - Import/Export queue to `.json` or `.txt`.
  - Drag-drop Spotify links directly into the app.
  - Since v0.7: Queue persistence (auto-save/restore on restart).
  - Since v0.7: Retry failed and Remove completed actions.

- Download Control
  - Choose format: `flac`, `mp3`, `m4a`, `opus`.
  - Parallel downloads with adaptive rate limiting.
  - Pause/Resume queue, or stop after current job.
  - Logs with raw terminal output and per-job summaries.
  - Since v0.7: Windows taskbar progress + tray tooltip with elapsed/ETA.
  - Since v0.7: Shows installed `spotify-dl` version next to the binary pill.

- Background Modes
  - Minimize to tray with notifications.
  - Sentry Mode: auto-captures copied Spotify links and downloads them slowly in the background with a configurable gap (hands-off library building).
  - Scheduler: run your queue daily at a set time.

- Library Organization
  - Auto-organize into Album/Artist folders.
  - Duplicate handling (replace if larger, skip otherwise).
  - Optional cover image extraction.
  - Integrity checks: flag suspiciously small or incomplete files.
  - One-click reorganization of the destination folder from Settings.

- History & Logs
  - Keeps history of downloaded jobs.
  - Log file paths, sizes, suspect files.
  - JSON summaries for automation.
  - Since v0.7: History capacity setting (in Settings).
  - Since v0.7: Export visible history and Clear history actions.

- UI Goodies
  - Dark theme with orange highlights.
  - Persistent terminal (Windows) option.
  - Tray menu with Sentry toggle + Quit.
  - Clipboard auto-add toggle.
  - Since v0.7: Global keyboard shortcuts (press F1 for a full list).
  - Since v0.7: "Open in Spotify" from queue/history.

- Remote Control
  - Optional lightweight web server with configurable host/port/auth to submit Spotify links remotely (ideal for media servers).

---

## Screenshots

TODO: add screenshots (main window, settings, tray menu, sentry indicator)

---

## What’s New in v0.9.5

- Forward spotify-dl JSON events for realtime progress, retry, and rate-limit feedback.
- Optional remote web server to queue Spotify links from other devices (with auth & media-folder override).
- Queue persistence, Retry failed, and Remove completed actions.
- Keyboard shortcuts with a Shortcuts dialog (press F1).
- Binary pill now shows the installed `spotify-dl` version.
- Configurable History capacity; export visible entries and clear history.
- Windows: taskbar progress, tray tooltip with elapsed/ETA, and auto pacing in Sentry mode.
- "Open in Spotify" from queue context menu and History.

---

## Contributing

Pull requests welcome! If you have ideas for features, open an issue or PR.

---

## License

MIT — see [LICENSE](LICENSE) for details.
