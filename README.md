# spotify-dl GUI

A modern, dark-themed desktop GUI wrapper for [spotify-dl](https://github.com/GuillemCastro/spotify-dl), built with Python + PySide6.
Easily download your Spotify playlists, albums, and tracks with one click, organize your library automatically, and run in the background with Sentry mode.

Current version: v0.7

---

## Features

- Queue Management
  - Add multiple Spotify links (tracks, albums, playlists).
  - Reorder, remove, or clear queue items.
  - Import/Export queue to `.json` or `.txt`.
  - Drag-drop Spotify links directly into the app.
  - New in v0.7: Queue persistence (auto-save/restore on restart).
  - New in v0.7: Retry failed and Remove completed actions.

- Download Control
  - Choose format: `flac`, `mp3`, `m4a`, `opus`.
  - Parallel downloads with adaptive rate limiting.
  - Pause/Resume queue, or stop after current job.
  - Logs with raw terminal output and per-job summaries.
  - New in v0.7: Windows taskbar progress + tray tooltip with elapsed/ETA.
  - New in v0.7: Shows installed `spotify-dl` version next to the binary pill.

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
  - New in v0.7: History capacity setting (in Settings).
  - New in v0.7: Export visible history and Clear history actions.

- UI Goodies
  - Dark theme with orange highlights.
  - Persistent terminal (Windows) option.
  - Tray menu with Sentry toggle + Quit.
  - Clipboard auto-add toggle.
  - New in v0.7: Global keyboard shortcuts (press F1 for a full list).
  - New in v0.7: “Open in Spotify” from queue/history.

---

## Screenshots

TODO: add screenshots (main window, settings, tray menu, sentry indicator)

---

## Getting Started

### Prerequisites
- Python 3.10+ (tested with 3.11)
- [spotify-dl](https://github.com/GuillemCastro/spotify-dl) installed (`cargo install spotify-dl` or use provided binary)
- Spotify Premium account (required by spotify-dl)

### Installation

Clone this repo:

```bash
git clone https://github.com/yourusername/spotify-dl-gui.git
cd spotify-dl-gui
```

Create a virtual environment & install dependencies:

```bash
python -m venv .venv
.\.venv\Scripts\activate  # (Windows)
# or source .venv/bin/activate (Linux/macOS)

pip install -r requirements.txt
```

### Run

```bash
python -m spotifydl_gui
```

The app should open with the dark-themed GUI. On first run, `spotify-dl` will prompt you to log in via your terminal.

---

## Packaging

To build an executable for distribution, you can use [PyInstaller](https://pyinstaller.org/):

```bash
pip install pyinstaller
pyinstaller --name "spotify-dl-gui" --icon spotify-dl-gui.ico --noconsole -w spotifydl_gui/main.py
```

This will generate a standalone `spotify-dl-gui.exe` in `dist/`.

---

## Disclaimer

This tool is a community-built GUI for [spotify-dl](https://github.com/GuillemCastro/spotify-dl).
Use responsibly — downloading Spotify content may violate Spotify's Terms of Service and/or local copyright laws.
You are responsible for how you use this software.

---

## Roadmap

* [x] Queue system with pause/resume
* [x] History & logs
* [x] Dark theme with tray support
* [x] Sentry mode (hands-off clipboard capture)
* [x] One-click library reorganization
* [x] Queue persistence on restart
* [ ] Search/filter in queue
* [ ] Stats dashboard (library size, formats, duplicates)
* [ ] Auto-update check
* [ ] Headless mode (background service)

---

## What’s New in v0.7

- Queue now persists across restarts.
- Retry failed and Remove completed queue actions.
- Keyboard shortcuts with a Shortcuts dialog (press F1).
- Binary pill shows the installed `spotify-dl` version.
- Configurable History capacity; export visible entries and clear history.
- Windows: taskbar progress and tray tooltip with elapsed/ETA.
- “Open in Spotify” from queue context menu and History.

---

## Contributing

Pull requests welcome! If you have ideas for features, open an issue or PR.

---

## License

MIT — see [LICENSE](LICENSE) for details.

