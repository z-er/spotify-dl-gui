# main.py
# spotify-dl GUI — Dark theme + Settings + bundled-binary preference + persistent terminal (Windows)
# Organizer template + cover art extraction + duplicate handling
# History + per-job logs
# NOW: per-URL queue — one log & history entry per input URL
import sys, shlex, re, os, subprocess, platform, ctypes, time, shutil, json
from pathlib import Path
from shutil import which as _which
from datetime import datetime
from enum import Enum
from PySide6.QtCore import Qt, QProcess, QSettings, QTimer
from PySide6.QtGui import QPalette, QColor
from PySide6.QtWidgets import (
    QApplication, QWidget, QVBoxLayout, QHBoxLayout, QLabel, QLineEdit,
    QTextEdit, QPushButton, QFileDialog, QSpinBox, QComboBox, QMessageBox,
    QCheckBox, QFrame, QDialog, QDialogButtonBox, QListWidget, QListWidgetItem
)

# Tag reading
from mutagen import File as MutagenFile
from mutagen.id3 import ID3, TALB, APIC
from mutagen.mp4 import MP4, MP4Cover
from mutagen.flac import FLAC, Picture as FLACPicture

APP_ORG = "JoshTools"
APP_NAME = "spotify-dl GUI"
PERSISTENT_TITLE = "SpotifyDL GUI Terminal"
HISTORY_LIMIT = 100

# --- Queue throttle/backoff tuning ---
THROTTLE_TRACKS_THRESHOLD = 30       # if a single URL yields >= this many tracks, throttle before next URL
BACKOFF_SEQUENCE_SECONDS = [10, 20, 30]  # progressive delays on consecutive failures
BACKOFF_RESET_ON_SUCCESS = True
CLIPBOARD_POLL_MS = 1000             # clipboard watcher tick

def which(cmd: str):
    return _which(cmd)

SPOTIFY_URL_RE = re.compile(
    r"^(https?://open\.spotify\.com/(track|album|playlist)/[A-Za-z0-9]+(\?.*)?$|spotify:(track|album|playlist):[A-Za-z0-9]+)$",
    re.IGNORECASE,
)

class QStatus(str, Enum):
    PENDING = "Pending"
    RUNNING = "Running"
    OK      = "Done"
    FAIL    = "Failed"
    PAUSED  = "Paused"
    SKIPPED = "Skipped"

# Optional: simple emoji badges (easy + readable on dark theme)
QUEUE_BADGE = {
    QStatus.PENDING: "⏳",
    QStatus.RUNNING: "▶️",
    QStatus.OK:      "✅",
    QStatus.FAIL:    "❌",
    QStatus.PAUSED:  "⏸️",
    QStatus.SKIPPED: "⤼",
}

class Line(QFrame):
    def __init__(self):
        super().__init__()
        self.setFrameShape(QFrame.HLine)
        self.setFrameShadow(QFrame.Sunken)
        self.setStyleSheet("color: #2a2f39;")

def apply_dark_theme(app: QApplication):
    bg         = QColor("#0f131a")
    bg_alt     = QColor("#141a22")
    surface    = QColor("#1a212b")
    border     = QColor("#2a2f39")
    text       = QColor("#e6eaf2")
    text_muted = QColor("#b5bcc9")
    accent     = QColor("#f4a261")
    danger     = QColor("#f7768e")

    pal = QPalette()
    pal.setColor(QPalette.Window, bg)
    pal.setColor(QPalette.WindowText, text)
    pal.setColor(QPalette.Base, surface)
    pal.setColor(QPalette.AlternateBase, bg_alt)
    pal.setColor(QPalette.ToolTipBase, surface)
    pal.setColor(QPalette.ToolTipText, text)
    pal.setColor(QPalette.Text, text)
    pal.setColor(QPalette.Button, bg_alt)
    pal.setColor(QPalette.ButtonText, text)
    pal.setColor(QPalette.BrightText, danger)
    pal.setColor(QPalette.Link, accent)
    pal.setColor(QPalette.Highlight, accent)
    pal.setColor(QPalette.HighlightedText, QColor("#0b0f16"))
    pal.setColor(QPalette.PlaceholderText, text_muted)
    app.setPalette(pal)

    check_svg = (
        "url(\"data:image/svg+xml;utf8,"
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 20'>"
        "<path d='M5 10.5 8.5 14 15 6' fill='none' stroke='%23ffffff' stroke-width='2.2' "
        "stroke-linecap='round' stroke-linejoin='round'/>"
        "</svg>\")"
    )

    app.setStyleSheet(f"""
        QWidget {{
            background: {bg.name()};
            color: {text.name()};
            font-family: Segoe UI, Inter, -apple-system, Arial;
            font-size: 12.5px;
        }}

        QLabel {{ color: {text.name()}; }}
        .muted {{ color: {text_muted.name()}; }}

        QLineEdit, QTextEdit, QComboBox, QSpinBox {{
            background: {surface.name()};
            color: {text.name()};
            border: 1px solid {border.name()};
            border-radius: 10px;
            padding: 8px;
            selection-background-color: {accent.name()};
            selection-color: #0b0f16;
        }}
        QLineEdit:focus, QTextEdit:focus, QComboBox:focus, QSpinBox:focus {{
            border-color: {accent.name()};
            box-shadow: 0 0 0 2px {accent.name()}33;
        }}
        QTextEdit {{ padding: 8px; }}

        QPushButton {{
            background: {bg_alt.name()};
            color: {text.name()};
            border: 1px solid {border.name()};
            border-radius: 10px;
            padding: 9px 14px;
        }}
        QPushButton:hover {{ background: {surface.name()}; }}
        QPushButton:pressed {{ background: {border.name()}; }}
        QPushButton:disabled {{
            color: {text_muted.name()};
            border-color: {border.name()};
            background: {bg.name()};
        }}

        QCheckBox, QRadioButton {{ color: {text.name()}; spacing: 8px; }}
        QCheckBox::indicator, QRadioButton::indicator {{ width: 18px; height: 18px; }}

        QCheckBox::indicator {{
            border: 1px solid {border.name()};
            border-radius: 5px;
            background: {surface.name()};
        }}
        QCheckBox::indicator:hover {{ border-color: {accent.name()}; }}
        QCheckBox::indicator:checked {{
            background: {accent.name()};
            border-color: {accent.name()};
            image: {check_svg};
        }}

        QComboBox::drop-down {{ border: none; width: 24px; }}
        QComboBox QAbstractItemView {{
            background: {bg_alt.name()};
            color: {text.name()};
            border: 1px solid {border.name()};
            selection-background-color: {accent.name()};
            selection-color: #0b0f16;
        }}

        QScrollBar:vertical {{
            background: {bg.name()};
            width: 12px;
            margin: 2px;
        }}
        QScrollBar::handle:vertical {{
            background: {surface.name()};
            border: 1px solid {border.name()};
            border-radius: 6px;
            min-height: 24px;
        }}
        QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{ height: 0px; }}

        QToolTip {{
            background: {surface.name()};
            color: {text.name()};
            border: 1px solid {border.name()};
            padding: 6px 8px;
            border-radius: 8px;
        }}
    """)

# ------- Windows console helpers -------
def _console_hwnd_for_pid(pid: int):
    if platform.system() != "Windows":
        return None
    EnumWindows = ctypes.windll.user32.EnumWindows
    GetWindowThreadProcessId = ctypes.windll.user32.GetWindowThreadProcessId
    IsWindowVisible = ctypes.windll.user32.IsWindowVisible
    GetClassNameW = ctypes.windll.user32.GetClassNameW

    hwnds = []
    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def callback(hwnd, lParam):
        if not IsWindowVisible(hwnd):
            return True
        pid_out = ctypes.c_ulong()
        GetWindowThreadProcessId(hwnd, ctypes.byref(pid_out))
        if pid_out.value == pid:
            buf = ctypes.create_unicode_buffer(256)
            GetClassNameW(hwnd, buf, 256)
            if buf.value == "ConsoleWindowClass":
                hwnds.append(hwnd)
        return True
    EnumWindows(callback, 0)
    return hwnds[0] if hwnds else None

def _show_window(hwnd, show=True):
    if platform.system() != "Windows" or not hwnd:
        return
    SW_SHOW, SW_HIDE = 5, 0
    ctypes.windll.user32.ShowWindow(hwnd, SW_SHOW if show else SW_HIDE)
    if show:
        ctypes.windll.user32.SetForegroundWindow(hwnd)

# ------- Settings Dialog -------
class SettingsDialog(QDialog):
    def __init__(self, parent, settings: QSettings):
        super().__init__(parent)
        self.setWindowTitle("Settings")
        self.setMinimumWidth(620)
        self.settings = settings

        # General
        self.open_when_done_chk = QCheckBox("Open destination folder when done")

        # Organizer template
        self.organize_chk = QCheckBox("Rearrange downloaded files using folder template")
        self.template_edit = QLineEdit()
        self.template_edit.setPlaceholderText("{artist}/{album}")
        self.template_preview = QLabel("Preview: ")
        self.template_preview.setProperty("class", "muted")
        self.template_help = QLabel("Tokens: {artist}, {album}, {title}, {track}, {disc}, {year}, {ext}, {filename}  (format OK: {track:02d})")
        self.template_help.setProperty("class", "muted")
        self.template_edit.textChanged.connect(self._update_preview)

        # Duplicate handling
        self.dup_resolve_chk = QCheckBox("Resolve duplicates by size (keep larger)")
        self.dup_delete_smaller_chk = QCheckBox("Delete smaller/equals when duplicate (otherwise skip)")

        # Cover art extraction
        self.cover_extract_chk = QCheckBox("Extract embedded cover art into folder (cover.jpg/png if missing)")

        # Persistent terminal
        self.persistent_terminal_chk = QCheckBox("Enable persistent terminal (Windows)")
        if platform.system() != "Windows":
            self.persistent_terminal_chk.setEnabled(False)
            self.persistent_terminal_chk.setToolTip("Windows only")

        # Binary picker
        self.bin_edit = QLineEdit()
        self.bin_edit.setPlaceholderText("Path to spotify-dl (optional; leave blank to auto-detect)")
        self.btn_bin = QPushButton("Browse")
        self.btn_bin.clicked.connect(self.pick_bin)

        row_bin = QHBoxLayout()
        row_bin.addWidget(self.bin_edit, 1)
        row_bin.addWidget(self.btn_bin)

        info = QLabel(
            "Binary resolution order:\n"
            "1) Bundled executable next to this app (preferred)\n"
            "2) Custom path set here\n"
            "3) System PATH (`spotify-dl` / `spotify-dl.exe`)"
        )
        info.setProperty("class", "muted")

        # Buttons
        btns = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        btns.accepted.connect(self.accept)
        btns.rejected.connect(self.reject)

        # Layout
        layout = QVBoxLayout(self)
        layout.addWidget(QLabel("General"))
        layout.addWidget(self.open_when_done_chk)
        layout.addSpacing(6)

        layout.addWidget(QLabel("Download Organization"))
        layout.addWidget(self.organize_chk)
        layout.addWidget(self.template_edit)
        layout.addWidget(self.template_preview)
        layout.addWidget(self.template_help)
        layout.addSpacing(6)

        layout.addWidget(QLabel("Duplicate Handling"))
        layout.addWidget(self.dup_resolve_chk)
        layout.addWidget(self.dup_delete_smaller_chk)
        layout.addSpacing(6)

        layout.addWidget(QLabel("Cover Art"))
        layout.addWidget(self.cover_extract_chk)
        layout.addSpacing(6)

        layout.addWidget(QLabel("Persistent Terminal"))
        layout.addWidget(self.persistent_terminal_chk)
        layout.addSpacing(6)

        layout.addWidget(QLabel("spotify-dl Binary"))
        layout.addLayout(row_bin)
        layout.addWidget(info)
        layout.addSpacing(8)
        layout.addWidget(btns)

        # Load settings
        self.open_when_done_chk.setChecked(settings.value("open_when_done", "false") == "true")
        self.persistent_terminal_chk.setChecked(settings.value("persistent_terminal", "false") == "true")
        self.organize_chk.setChecked(settings.value("organize_enabled", "true") == "true")
        self.template_edit.setText(settings.value("template", "{artist}/{album}"))
        self.dup_resolve_chk.setChecked(settings.value("dup_resolve", "true") == "true")
        self.dup_delete_smaller_chk.setChecked(settings.value("dup_delete_smaller", "false") == "true")
        self.cover_extract_chk.setChecked(settings.value("cover_extract", "true") == "true")
        self.bin_edit.setText(settings.value("bin", ""))
        self._update_preview()

    def _update_preview(self):
        demo = {
            "artist": "David Bowie",
            "album": "Heroes",
            "title": "Heroes",
            "track": 1,
            "disc": 1,
            "year": 1977,
            "ext": ".flac",
            "filename": "David Bowie - Heroes.flac",
        }
        class FmtDict(dict):
            def __missing__(self, k): return "{"+k+"}"
        try:
            preview = self.template_edit.text().format_map(FmtDict(demo)).replace("//", "/").strip("/\\")
        except Exception:
            preview = "(invalid template)"
        self.template_preview.setText(f"Preview: {preview or '(root)'}")

    def pick_bin(self):
        path, _ = QFileDialog.getOpenFileName(self, "Select spotify-dl executable")
        if path:
            self.bin_edit.setText(path)

    def apply(self):
        self.settings.setValue("open_when_done", "true" if self.open_when_done_chk.isChecked() else "false")
        self.settings.setValue("persistent_terminal", "true" if self.persistent_terminal_chk.isChecked() else "false")
        self.settings.setValue("organize_enabled", "true" if self.organize_chk.isChecked() else "false")
        self.settings.setValue("template", self.template_edit.text().strip())
        self.settings.setValue("dup_resolve", "true" if self.dup_resolve_chk.isChecked() else "false")
        self.settings.setValue("dup_delete_smaller", "true" if self.dup_delete_smaller_chk.isChecked() else "false")
        self.settings.setValue("cover_extract", "true" if self.cover_extract_chk.isChecked() else "false")
        self.settings.setValue("bin", self.bin_edit.text().strip())

# ------- History Dialog -------
class HistoryDialog(QDialog):
    def __init__(self, parent, history: list[dict]):
        super().__init__(parent)
        self.setWindowTitle("History")
        self.setMinimumSize(700, 440)
        self.history = history

        self.list = QListWidget()
        for job in history:
            ts = job.get("start_iso", "?")
            dest = job.get("dest", "")
            code = job.get("code", -1)
            moved = job.get("moved", 0)
            replaced = job.get("replaced", 0)
            deleted = job.get("deleted", 0)
            skipped = job.get("skipped", 0)
            artist = job.get("first_artist","")
            album = job.get("first_album","")
            status = "OK" if code == 0 else f"Exit {code}"
            label = f"[{ts}]  {status}  → {dest}"
            if artist or album:
                label += f"   ({artist} – {album})"
            label += f"   (moved:{moved} repl:{replaced} del:{deleted} skip:{skipped})"
            item = QListWidgetItem(label)
            item.setData(Qt.UserRole, job)
            self.list.addItem(item)

        self.btn_open_log = QPushButton("Open selected log")
        self.btn_open_log.clicked.connect(self.open_log)
        self.btn_open_dest = QPushButton("Open selected folder")
        self.btn_open_dest.clicked.connect(self.open_dest)

        btns = QDialogButtonBox(QDialogButtonBox.Close)
        btns.button(QDialogButtonBox.Close).clicked.connect(self.close)

        row = QHBoxLayout()
        row.addWidget(self.btn_open_log)
        row.addSpacing(8)
        row.addWidget(self.btn_open_dest)
        row.addStretch()

        layout = QVBoxLayout(self)
        layout.addWidget(self.list)
        layout.addLayout(row)
        layout.addWidget(btns)

    def _selected_job(self):
        it = self.list.currentItem()
        return it.data(Qt.UserRole) if it else None

    def open_log(self):
        job = self._selected_job()
        if not job:
            return
        p = job.get("log_path", "")
        if p and Path(p).exists():
            try:
                if sys.platform.startswith("win"):
                    os.startfile(p)  # type: ignore
                elif sys.platform == "darwin":
                    subprocess.Popen(["open", p])
                else:
                    subprocess.Popen(["xdg-open", p])
            except Exception:
                pass

    def open_dest(self):
        job = self._selected_job()
        if not job:
            return
        d = job.get("dest", "")
        if d and Path(d).exists():
            try:
                if sys.platform.startswith("win"):
                    os.startfile(d)  # type: ignore
                elif sys.platform == "darwin":
                    subprocess.Popen(["open", d])
                else:
                    subprocess.Popen(["xdg-open", d])
            except Exception:
                pass

# ------- Queue Row -------
class QueueRow(QWidget):
    def __init__(self, url: str, status: QStatus = QStatus.PENDING):
        super().__init__()
        self.url = url
        self.status = status

        self.lbl_badge = QLabel(QUEUE_BADGE[status])
        self.lbl_badge.setFixedWidth(24)

        self.lbl_url = QLabel(url)
        self.lbl_url.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.lbl_url.setWordWrap(False)
        self.lbl_url.setMinimumWidth(280)
        self.lbl_url.setToolTip(url)
        self.lbl_status = QLabel(status.value)
        self.lbl_status.setProperty("class", "muted")

        row = QHBoxLayout(self)
        row.setContentsMargins(8, 6, 8, 6)
        row.setSpacing(10)
        row.addWidget(self.lbl_badge)
        row.addWidget(self.lbl_url, 1)
        row.addWidget(self.lbl_status)

        # subtle capsule background
        self.setStyleSheet("""
            QWidget {
                background: rgba(255,255,255,0.02);
                border: 1px solid #2a2f39;
                border-radius: 10px;
            }
        """)

    def set_status(self, status: QStatus):
        self.status = status
        self.lbl_badge.setText(QUEUE_BADGE[status])
        self.lbl_status.setText(status.value)
        # color hint
        color = {
            QStatus.PENDING: "#b5bcc9",
            QStatus.RUNNING: "#f4a261",
            QStatus.OK:      "#8ad7a0",
            QStatus.FAIL:    "#f7768e",
            QStatus.PAUSED:  "#f4a261",
            QStatus.SKIPPED: "#b5bcc9",
        }[status]
        self.lbl_status.setStyleSheet(f"color: {color};")

# ------- Main App -------
class SpotifyDLGui(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle(APP_NAME)
        self.setMinimumSize(860, 640)

        self.settings = QSettings(APP_ORG, APP_NAME)

        # Header
        header = QLabel("spotify-dl — simple GUI")
        header.setStyleSheet("font-size: 18px; font-weight: 600;")

        self.settings_btn = QPushButton("Settings")
        self.settings_btn.clicked.connect(self.open_settings)

        self.history_btn = QPushButton("History")
        self.history_btn.clicked.connect(self.open_history)

        header_row = QHBoxLayout()
        header_row.addWidget(header)
        header_row.addStretch()
        header_row.addWidget(self.history_btn)
        header_row.addWidget(self.settings_btn)

        # Queue control
        self._paused: bool = False
        self._pending_delay_ms: int = 0
        self._backoff_index: int = 0        # which delay in BACKOFF_SEQUENCE_SECONDS we’re on
        self._consecutive_failures: int = 0

        # (used by throttle decision)
        self._last_job_output_count: int = 0

        # Clipboard watcher
        self._last_clip_seen: str = ""
        self._clip_timer = QTimer(self)
        self._clip_timer.setInterval(CLIPBOARD_POLL_MS)
        self._clip_timer.timeout.connect(self._poll_clipboard)
        self._clip_timer.start()

        # Inputs
        self.urls_edit = QTextEdit()
        self.urls_edit.setPlaceholderText("Paste Spotify URLs or URIs (one per line)")
        self.urls_edit.setAcceptRichText(False)

        self.dest_edit = QLineEdit()
        self.dest_edit.setPlaceholderText("Choose a destination folder")
        btn_dest = QPushButton("Browse")
        btn_dest.clicked.connect(self.pick_dest)

        dest_row = QHBoxLayout()
        dest_row.addWidget(QLabel("Destination"))
        dest_row.addSpacing(8)
        dest_row.addWidget(self.dest_edit, 1)
        dest_row.addWidget(btn_dest)

        self.format_combo = QComboBox()
        self.format_combo.addItems(["flac", "mp3", "m4a", "opus"])
        self.parallel_spin = QSpinBox()
        self.parallel_spin.setRange(1, 32)
        self.parallel_spin.setValue(5)
        self.force_chk = QCheckBox("Force re-download")

        opts_row = QHBoxLayout()
        opts_row.addWidget(QLabel("Format"))
        opts_row.addWidget(self.format_combo)
        opts_row.addSpacing(16)
        opts_row.addWidget(QLabel("Parallel"))
        opts_row.addWidget(self.parallel_spin)
        opts_row.addStretch()

        self.extra_args = QLineEdit()
        self.extra_args.setPlaceholderText("Optional extra flags (advanced) e.g. --something")

        # Controls
        self.toggle_terminal_btn = QPushButton("Show/Hide terminal")
        self.toggle_terminal_btn.clicked.connect(self.toggle_terminal)
        if platform.system() != "Windows":
            self.toggle_terminal_btn.setEnabled(False)
            self.toggle_terminal_btn.setToolTip("Windows only")

        self.run_btn = QPushButton("Run")
        self.run_btn.clicked.connect(self.start_queue)
        self.run_btn.setDefault(True)

        self.stop_btn = QPushButton("Stop")
        self.stop_btn.clicked.connect(self.stop_queue)
        self.stop_btn.setEnabled(False)

        # Add-from-clipboard (appears when a Spotify link is detected in clipboard)
        self.add_clip_btn = QPushButton("Add from clipboard")
        self.add_clip_btn.setToolTip("Paste the Spotify link detected in your clipboard")
        self.add_clip_btn.clicked.connect(self._add_clipboard_url)
        self.add_clip_btn.setVisible(False)  # shown only when we detect a new link

        # Pause/Resume toggle (applies after the current URL finishes)
        self.pause_btn = QPushButton("Pause after current")
        self.pause_btn.setToolTip("Finish the current URL, then pause the queue")
        self.pause_btn.clicked.connect(self._toggle_pause)
        self.pause_btn.setEnabled(False)

        ctrl_row = QHBoxLayout()
        ctrl_row.addWidget(self.toggle_terminal_btn)
        ctrl_row.addStretch()
        ctrl_row.addWidget(self.add_clip_btn)
        ctrl_row.addStretch()
        ctrl_row.addWidget(self.pause_btn)
        ctrl_row.addWidget(self.run_btn)
        ctrl_row.addWidget(self.stop_btn)

        disclaimer = QLabel("Requires Spotify Premium. Use at your own risk — may violate Spotify Terms or local laws.")
        disclaimer.setWordWrap(True)
        disclaimer.setProperty("class", "muted")
        
        # Layout
        layout = QVBoxLayout(self)
        layout.addLayout(header_row)
        layout.addSpacing(4)
        layout.addWidget(Line())
        layout.addSpacing(4)
        layout.addWidget(QLabel("Tracks / Playlists / Albums"))
        layout.addWidget(self.urls_edit, 1)
        layout.addLayout(dest_row)
        layout.addSpacing(4)
        layout.addLayout(opts_row)
        layout.addSpacing(6)
        layout.addWidget(QLabel("Extra flags"))
        layout.addWidget(self.extra_args)
        layout.addSpacing(8)
        layout.addWidget(Line())
        layout.addLayout(ctrl_row)
        layout.addSpacing(6)
        layout.addWidget(disclaimer)

        # --- Footer pill ---
        self.bin_pill = QLabel()
        self.bin_pill.setObjectName("binPill")
        self.bin_pill.setTextInteractionFlags(Qt.TextSelectableByMouse)

        footer = QHBoxLayout()
        footer.addStretch()
        footer.addWidget(self.bin_pill)

        layout.addSpacing(6)
        layout.addLayout(footer)

        # initialize pill
        self.update_bin_pill()

        # State
        self.proc_qt: QProcess | None = None
        self.persistent_proc: subprocess.Popen | None = None
        self.persistent_hwnd = None
        self.persistent_inited_for_login = False

        # Queue state
        self._queue_urls: list[str] = []
        self._queue_index: int = 0
        self._queue_agg = {"moved":0, "replaced":0, "deleted":0, "skipped":0, "ok":0, "fail":0}

        # Per-URL job state
        self._pre_run_files: set[str] | None = None
        self._run_started_at: float | None = None
        self._log_buffer: list[str] = []
        self._job_outputs: list[dict] = []
        self._job_start_iso: str = ""
        self._current_url: str = ""
        self._job_stats = {"moved":0, "replaced":0, "deleted":0, "skipped":0}

        # Load persisted fields
        self.dest_edit.setText(self.settings.value("dest", ""))
        self.format_combo.setCurrentText(self.settings.value("format", "flac"))
        self.parallel_spin.setValue(int(self.settings.value("parallel", 5)))
        self.force_chk.setChecked(self.settings.value("force", "false") == "true")
        self.extra_args.setText(self.settings.value("extra", ""))

        if platform.system() == "Windows" and self.settings.value("persistent_terminal", "false") == "true":
            self.ensure_persistent_terminal(start_hidden=True)

    # ---------- Settings / History ----------
    def open_settings(self):
        dlg = SettingsDialog(self, self.settings)
        if dlg.exec() == QDialog.Accepted:
            dlg.apply()
            self.update_bin_pill()
            if platform.system() == "Windows" and self.settings.value("persistent_terminal", "false") == "true":
                self.ensure_persistent_terminal(start_hidden=True)

    def _load_history(self) -> list[dict]:
        try:
            data = self.settings.value("history", "[]")
            return json.loads(data)
        except Exception:
            return []

    def _save_history(self, history: list[dict]):
        try:
            self.settings.setValue("history", json.dumps(history[-HISTORY_LIMIT:]))
        except Exception:
            pass

    def open_history(self):
        hist = self._load_history()
        HistoryDialog(self, hist).exec()

    # ---------- Field persistence ----------
    def save_main_fields(self):
        self.settings.setValue("dest", self.dest_edit.text().strip())
        self.settings.setValue("format", self.format_combo.currentText())
        self.settings.setValue("parallel", self.parallel_spin.value())
        self.settings.setValue("force", "true" if self.force_chk.isChecked() else "false")
        self.settings.setValue("extra", self.extra_args.text().strip())

    # ---------- Helpers ----------
    def pick_dest(self):
        path = QFileDialog.getExistingDirectory(self, "Choose destination folder")
        if path:
            self.dest_edit.setText(path)

    def resolve_binary(self) -> str:
        base_dir = Path(sys.executable).parent if getattr(sys, "frozen", False) else Path(__file__).parent
        candidate_names = ["spotify-dl.exe", "spotify-dl"] if platform.system() == "Windows" else ["spotify-dl"]
        for name in candidate_names:
            p = base_dir / name
            if p.exists() and p.is_file():
                return str(p)
        custom = self.settings.value("bin", "").strip()
        if custom:
            cp = Path(custom)
            if cp.exists() and cp.is_file():
                return str(cp)
        exe = which("spotify-dl") or which("spotify-dl.exe")
        if exe:
            return exe
        raise RuntimeError("spotify-dl executable not found.\n"
                           "Place it next to this app, set a custom path in Settings, or add to PATH.")
    
    # --- Binary detection + footer pill update ---
    def _detect_binary(self) -> tuple[str | None, str]:
        """
        Returns: (path or None, source_label in {'Bundled','Custom','PATH','Not found'})
        Mirrors resolve_binary() logic but won't raise.
        """
        try:
            base_dir = Path(sys.executable).parent if getattr(sys, "frozen", False) else Path(__file__).parent
            candidate_names = ["spotify-dl.exe", "spotify-dl"] if platform.system() == "Windows" else ["spotify-dl"]
            for name in candidate_names:
                p = base_dir / name
                if p.exists() and p.is_file():
                    return str(p), "Bundled"

            custom = (self.settings.value("bin", "") or "").strip()
            if custom:
                cp = Path(custom)
                if cp.exists() and cp.is_file():
                    return str(cp), "Custom"

            exe = which("spotify-dl") or which("spotify-dl.exe")
            if exe:
                return exe, "PATH"
            return None, "Not found"
        except Exception:
            return None, "Not found"

    def update_bin_pill(self):
        """
        Updates the footer pill text + style.
        """
        path, source = self._detect_binary()
        ok = path is not None

        # Short display; full path in tooltip
        if ok:
            short = Path(path).name
            text = f"Binary: {source} — {short}"
            tooltip = path
        else:
            text = "Binary: Not found"
            tooltip = ("Place 'spotify-dl(.exe)' next to the app, or set a custom path in Settings, "
                    "or add it to PATH.")

        self.bin_pill.setText(text)
        self.bin_pill.setToolTip(tooltip)

        # Style the pill (green-ish when OK, red-ish when missing)
        if ok:
            bg = "#1b2a22"   # subtle green tint
            border = "#2a2f39"
            textc = "#e6eaf2"
        else:
            bg = "#2a1d1d"   # subtle red tint
            border = "#f7768e"
            textc = "#fbcaca"

        self.bin_pill.setStyleSheet(f"""
            QLabel#binPill {{
                background: {bg};
                color: {textc};
                border: 1px solid {border};
                border-radius: 9999px;
                padding: 6px 10px;
            }}
        """)
    
    def _poll_clipboard(self):
        """Detect Spotify link in clipboard and light up the button."""
        cb = QApplication.clipboard().text().strip()
        if not cb:
            self.add_clip_btn.setVisible(False)
            return
        # Only react to a new matching link
        if cb == self._last_clip_seen:
            return
        if SPOTIFY_URL_RE.match(cb):
            # Check it isn't already present in the URLs box
            existing = [ln.strip() for ln in self.urls_edit.toPlainText().splitlines() if ln.strip()]
            if cb not in existing:
                self.add_clip_btn.setVisible(True)
                self._last_clip_seen = cb
                self.add_clip_btn.setToolTip(f"Add: {cb}")
                return
        # fallback: hide if not a new spotify link
        self.add_clip_btn.setVisible(False)

    def _add_clipboard_url(self):
        """Append detected link into the URLs box."""
        if not self._last_clip_seen:
            return
        txt = self.urls_edit.toPlainText().rstrip()
        new_line = ("\n" if txt else "") + self._last_clip_seen
        self.urls_edit.setPlainText(txt + new_line)
        # Place cursor at end
        c = self.urls_edit.textCursor()
        c.movePosition(c.End)
        self.urls_edit.setTextCursor(c)
        # hide until a new clipboard value appears
        self.add_clip_btn.setVisible(False)

    def _toggle_pause(self):
        """Pause/resume the queue (takes effect between URLs)."""
        self._paused = not self._paused
        if self._paused:
            self.pause_btn.setText("Resume queue")
            self.pause_btn.setToolTip("Resume processing the remaining URLs")
        else:
            self.pause_btn.setText("Pause after current")
            self.pause_btn.setToolTip("Finish the current URL, then pause the queue")
            # If we were waiting to start the next job, kick it off
            if not self.proc_qt:
                self._maybe_start_next_after_delay()
    
    def _schedule_next_job(self, delay_seconds: int = 0):
        """Schedule the next job after an optional delay. Handles paused state."""
        self._pending_delay_ms = max(0, int(delay_seconds * 1000))
        # If paused, don't start—just keep the delay value; we’ll start on resume.
        if self._paused:
            return
        self._maybe_start_next_after_delay()

    def _maybe_start_next_after_delay(self):
        """Start the next job immediately or after the pending delay, unless paused."""
        if self._paused:
            return
        if self._pending_delay_ms > 0:
            delay = self._pending_delay_ms
            self._pending_delay_ms = 0
            QTimer.singleShot(delay, self._start_next_job)
        else:
            self._start_next_job()

    # ---------- Persistent terminal (Windows) ----------
    def ensure_persistent_terminal(self, start_hidden=False):
        if platform.system() != "Windows": return
        if self.settings.value("persistent_terminal", "false") != "true": return
        if self.persistent_proc and self.persistent_proc.poll() is None:
            if self.persistent_hwnd and start_hidden: _show_window(self.persistent_hwnd, False)
            return
        creation = subprocess.CREATE_NEW_CONSOLE | subprocess.CREATE_NEW_PROCESS_GROUP
        try:
            self.persistent_proc = subprocess.Popen(
                ["cmd.exe", "/k", f"title {PERSISTENT_TITLE}"],
                creationflags=creation,
                stdin=subprocess.PIPE,
                cwd=os.getcwd(),
                close_fds=False
            )
        except Exception as e:
            QMessageBox.critical(self, "Persistent terminal", f"Failed to open terminal:\n{e}")
            self.persistent_proc = None; self.persistent_hwnd = None; return
        self.persistent_hwnd = None
        def _grab_hwnd():
            if not self.persistent_proc: return
            hwnd = _console_hwnd_for_pid(self.persistent_proc.pid)
            if hwnd:
                self.persistent_hwnd = hwnd
                _show_window(hwnd, not start_hidden)
        QTimer.singleShot(300, _grab_hwnd)
        if not self.persistent_inited_for_login:
            try:
                exe = self.resolve_binary()
                self.send_to_persistent(exe)
                self.persistent_inited_for_login = True
            except Exception:
                pass

    def send_to_persistent(self, command: str):
        if platform.system() != "Windows": return
        if not self.persistent_proc or self.persistent_proc.poll() is not None:
            self.ensure_persistent_terminal(start_hidden=False)
        try:
            if self.persistent_proc and self.persistent_proc.stdin:
                self.persistent_proc.stdin.write((command + "\r\n").encode("utf-8", errors="ignore"))
                self.persistent_proc.stdin.flush()
        except Exception:
            pass

    def toggle_terminal(self):
        if platform.system() != "Windows": return
        if self.settings.value("persistent_terminal", "false") != "true":
            enable = QMessageBox.question(
                self, "Persistent terminal",
                "Enable persistent terminal in Settings and open it now?",
                QMessageBox.Yes | QMessageBox.No, QMessageBox.Yes
            )
            if enable == QMessageBox.Yes:
                self.settings.setValue("persistent_terminal", "true")
                self.ensure_persistent_terminal(start_hidden=False)
            return
        self.ensure_persistent_terminal(start_hidden=False)
        if self.persistent_hwnd: _show_window(self.persistent_hwnd, True)

    # ---------- Organizer / tags helpers ----------
    _AUDIO_EXTS = {".flac", ".mp3", ".m4a", ".mp4", ".opus", ".ogg", ".wav"}

    def _list_audio_files(self, root: Path) -> set[str]:
        files = set()
        if not root.exists(): return files
        for p in root.rglob("*"):
            if p.is_file() and p.suffix.lower() in self._AUDIO_EXTS:
                try: files.add(str(p.resolve()))
                except Exception: files.add(str(p))
        return files

    def _sanitize_component(self, s: str) -> str:
        bad = '<>:"/\\|?*'
        out = "".join("_" if c in bad else c for c in s).strip().strip(".")
        return out or "_"

    def _read_tags(self, path: Path) -> dict:
        tags = {
            "artist": "", "album": "", "title": path.stem,
            "track": 0, "disc": 1, "year": 0, "ext": path.suffix, "filename": path.name
        }
        ext = path.suffix.lower()
        try:
            if ext == ".mp3":
                id3 = ID3(str(path))
                if id3.get("TALB"): tags["album"] = str(id3.get("TALB").text[0])
                for t in ("TPE1","TPE2"):
                    fr = id3.get(t)
                    if fr and not tags["artist"]: tags["artist"] = str(fr.text[0])
                if id3.get("TIT2"): tags["title"] = str(id3.get("TIT2").text[0])
                if id3.get("TRCK"):
                    try: tags["track"] = int(str(id3.get("TRCK").text[0]).split("/")[0])
                    except: pass
                if id3.get("TPOS"):
                    try: tags["disc"] = int(str(id3.get("TPOS").text[0]).split("/")[0])
                    except: pass
                if id3.get("TDRC"):
                    try: tags["year"] = int(str(id3.get("TDRC").text[0])[:4])
                    except: pass
            elif ext in {".m4a", ".mp4"}:
                mp = MP4(str(path))
                def _get(k):
                    v = mp.tags.get(k); 
                    return v[0] if isinstance(v, list) and v else (v if v else "")
                alb = _get("\xa9alb"); art = _get("\xa9ART") or _get("aART"); ttl = _get("\xa9nam")
                trk = mp.tags.get("trkn"); dsk = mp.tags.get("disk"); day = _get("\xa9day")
                tags["album"] = alb or ""
                tags["artist"] = art or ""
                tags["title"] = ttl or tags["title"]
                if trk and trk[0] and trk[0][0]: tags["track"] = int(trk[0][0])
                if dsk and dsk[0] and dsk[0][0]: tags["disc"] = int(dsk[0][0])
                if day:
                    try: tags["year"] = int(str(day)[:4])
                    except: pass
            elif ext == ".flac":
                fl = FLAC(str(path))
                tags["album"]  = (fl.get("album",[ ""])[0] or "")
                tags["artist"] = (fl.get("artist",[""])[0] or fl.get("albumartist",[""])[0] if fl else "")
                tags["title"]  = (fl.get("title", [tags["title"]])[0])
                try: tags["track"] = int((fl.get("tracknumber",["0"])[0]).split("/")[0])
                except: pass
                try: tags["disc"] = int((fl.get("discnumber",["1"])[0]).split("/")[0])
                except: pass
                try: tags["year"] = int((fl.get("date",["0"])[0])[:4])
                except: pass
            else:
                mf = MutagenFile(str(path), easy=True)
                if mf and mf.tags:
                    def _get(key):
                        v = mf.tags.get(key)
                        return v[0] if isinstance(v, list) and v else (v if v else "")
                    tags["album"]  = _get("album") or ""
                    tags["artist"] = _get("artist") or _get("albumartist") or ""
                    tags["title"]  = _get("title") or tags["title"]
                    try: tags["track"] = int(str(_get("tracknumber")).split("/")[0])
                    except: pass
                    try: tags["disc"] = int(str(_get("discnumber")).split("/")[0])
                    except: pass
                    try: tags["year"] = int(str(_get("date"))[:4])
                    except: pass
        except Exception:
            pass
        if not tags["album"]: tags["album"] = "Unknown Album"
        if not tags["artist"]: tags["artist"] = "Unknown Artist"
        return tags

    def _compute_subfolder_from_template(self, path: Path, template: str) -> Path:
        tags = self._read_tags(path)
        class FmtDict(dict):
            def __missing__(self, k): return ""
        try: sub = template.format_map(FmtDict(tags))
        except Exception: sub = template
        parts = [self._sanitize_component(p) for p in re.split(r"[\\/]+", sub) if p != ""]
        return Path(*parts) if parts else Path()

    def _maybe_extract_cover(self, audio_path: Path, album_dir: Path):
        if self.settings.value("cover_extract", "true") != "true": return
        for name in ("cover.jpg", "cover.png", "folder.jpg", "folder.png"):
            if (album_dir / name).exists(): return
        try:
            ext = audio_path.suffix.lower()
            data = None; is_png = False
            if ext == ".mp3":
                id3 = ID3(str(audio_path))
                apics = id3.getall("APIC")
                if apics:
                    data = apics[0].data
                    is_png = apics[0].mime == "image/png" or (data[:8] == b"\x89PNG\r\n\x1a\n")
            elif ext in {".m4a", ".mp4"}:
                mp = MP4(str(audio_path))
                covr = mp.tags.get("covr")
                if covr:
                    pic = covr[0]
                    if isinstance(pic, MP4Cover):
                        data = bytes(pic)
                        is_png = (pic.imageformat == MP4Cover.FORMAT_PNG) or (data[:8] == b"\x89PNG\r\n\x1a\n")
            elif ext == ".flac":
                fl = FLAC(str(audio_path))
                if fl.pictures:
                    pic: FLACPicture = fl.pictures[0]
                    data = pic.data
                    is_png = pic.mime == "image/png" or (data[:8] == b"\x89PNG\r\n\x1a\n")
            if not data: return
            fname = "cover.png" if is_png else "cover.jpg"
            with open(album_dir / fname, "wb") as f:
                f.write(data)
        except Exception:
            pass

    def _replace_file(self, src: Path, dst: Path):
        tmp = dst.with_suffix(dst.suffix + ".tmp.replace")
        try:
            if dst.exists():
                try: dst.replace(tmp)
                except Exception:
                    try: dst.unlink()
                    except Exception: tmp = None
            src.replace(dst)
        finally:
            if tmp and tmp.exists():
                try: tmp.unlink()
                except Exception: pass

    def _record_output(self, tags: dict, final_path: Path, stats_inc_key=None):
        try: size = final_path.stat().st_size
        except Exception: size = -1
        self._job_outputs.append({
            "artist": tags.get("artist") or "",
            "title":  tags.get("title") or final_path.stem,
            "album":  tags.get("album") or "",
            "dest":   str(final_path),
            "size":   int(size),
        })
        self._last_job_output_count = len(self._job_outputs)
        if stats_inc_key:
            self._job_stats[stats_inc_key] += 1
            self._queue_agg[stats_inc_key] += 1

    def _organizer_move(self, file_path: Path, dest_root: Path) -> None:
        org_enabled = self.settings.value("organize_enabled", "true") == "true"
        template = self.settings.value("template", "{artist}/{album}").strip() or "{artist}/{album}"
        subfolder = self._compute_subfolder_from_template(file_path, template) if org_enabled else Path(self._sanitize_component(self._read_tags(file_path)["album"]))
        album_dir = dest_root / subfolder
        try:
            if album_dir.resolve() in file_path.resolve().parents:
                return
        except Exception:
            pass
        album_dir.mkdir(parents=True, exist_ok=True)
        target = album_dir / file_path.name

        dup_resolve = self.settings.value("dup_resolve", "true") == "true"
        dup_delete_smaller = self.settings.value("dup_delete_smaller", "false") == "true"

        if target.exists():
            if dup_resolve:
                try:
                    src_size = file_path.stat().st_size
                    dst_size = target.stat().st_size
                except Exception:
                    src_size = -1; dst_size = -1
                if src_size > dst_size:
                    temp_in_album = album_dir / (file_path.name + ".tmp.incoming")
                    try:
                        try:
                            shutil.move(str(file_path), str(temp_in_album))
                        except Exception:
                            shutil.copy2(str(file_path), str(temp_in_album))
                            file_path.unlink(missing_ok=True)
                        self._replace_file(temp_in_album, target)
                        self._record_output(self._read_tags(target), target, stats_inc_key="replaced")
                        self._maybe_extract_cover(target, album_dir)
                    finally:
                        if 'temp_in_album' in locals() and temp_in_album.exists():
                            try: temp_in_album.unlink()
                            except Exception: pass
                else:
                    if dup_delete_smaller:
                        file_path.unlink(missing_ok=True)
                        self._job_stats["deleted"] += 1
                        self._queue_agg["deleted"] += 1
                    else:
                        self._job_stats["skipped"] += 1
                        self._queue_agg["skipped"] += 1
                return
            else:
                stem, ext = file_path.stem, file_path.suffix
                n = 1
                while True:
                    cand = album_dir / f"{stem} ({n}){ext}"
                    if not cand.exists():
                        target = cand
                        break
                    n += 1

        try:
            shutil.move(str(file_path), str(target))
        except Exception:
            try:
                shutil.copy2(str(file_path), str(target))
                file_path.unlink(missing_ok=True)
            except Exception:
                pass
        else:
            self._record_output(self._read_tags(target), target, stats_inc_key="moved")
            self._maybe_extract_cover(target, album_dir)

    def _rearrange_new_downloads(self, dest_dir: str):
        if self.settings.value("organize_enabled", "true") != "true": return
        if not dest_dir: return
        root = Path(dest_dir)
        current = self._list_audio_files(root)
        pre = self._pre_run_files or set()
        new_candidates = {Path(p) for p in (current - pre)}
        if self._run_started_at:
            cutoff = self._run_started_at
            for p_str in current:
                p = Path(p_str)
                try:
                    if p.stat().st_mtime >= cutoff:
                        new_candidates.add(p)
                except Exception:
                    pass
        for p in new_candidates:
            try:
                self._organizer_move(p, root)
            except Exception:
                pass

    # ---------- History / logs ----------
    def _slug(self, s: str, maxlen: int = 40) -> str:
        s = re.sub(r"[^\w\-]+", "_", s.strip(), flags=re.UNICODE)
        s = re.sub(r"_+", "_", s).strip("_")
        return (s[:maxlen]).rstrip("_") or "log"

    def _write_log_file(self, dest_dir: str, url_index: int) -> str | None:
        try:
            if not dest_dir: return None
            logs_dir = Path(dest_dir) / "_logs"
            logs_dir.mkdir(parents=True, exist_ok=True)
            first = self._job_outputs[0] if self._job_outputs else {}
            artist_slug = self._slug(first.get("artist", "")) if first.get("artist") else ""
            album_slug  = self._slug(first.get("album", "")) if first.get("album") else ""
            ts = datetime.now().strftime("%Y%m%d_%H%M%S")
            base = f"run_{ts}_{url_index:02d}"
            if artist_slug or album_slug:
                base += "_" + "_".join([p for p in (artist_slug, album_slug) if p])
            log_path = logs_dir / f"{base}.txt"

            header_lines = []
            header_lines.append("=== spotify-dl GUI Job ===")
            header_lines.append(f"Started: {self._job_start_iso}")
            header_lines.append(f"Destination: {dest_dir}")
            header_lines.append(f"Input URL: {self._current_url}")
            header_lines.append("")
            header_lines.append(f"Outputs ({len(self._job_outputs)}):")
            for o in self._job_outputs:
                size_mb = f"{(o.get('size',0) / (1024*1024)):.2f} MB" if o.get('size',0) > 0 else "?"
                artist = o.get("artist",""); title=o.get("title",""); album=o.get("album",""); dest=o.get("dest","")
                header_lines.append(f"  - {artist} – {title} [{album}]")
                header_lines.append(f"    → {dest}  ({size_mb})")
            header_lines.append("")
            header_lines.append("=== Raw output ===")
            header_lines.append("")

            json_appendix = {
                "started": self._job_start_iso,
                "dest": dest_dir,
                "input": self._current_url,
                "outputs": self._job_outputs,
                "stats": self._job_stats,
            }

            with open(log_path, "w", encoding="utf-8", errors="ignore") as f:
                f.write("\n".join(header_lines))
                f.write("".join(self._log_buffer))
                f.write("\n\n=== Summary (JSON) ===\n")
                f.write(json.dumps(json_appendix, ensure_ascii=False, indent=2))
            return str(log_path)
        except Exception:
            return None

    def _append_history(self, code: int, dest: str, log_path: str | None):
        hist = self._load_history()
        first = (self._job_outputs[0] if self._job_outputs else {}) or {}
        hist.append({
            "start_iso": self._job_start_iso,
            "code": int(code),
            "dest": dest,
            "log_path": log_path or "",
            "urls": 1,
            "moved": self._job_stats.get("moved",0),
            "replaced": self._job_stats.get("replaced",0),
            "deleted": self._job_stats.get("deleted",0),
            "skipped": self._job_stats.get("skipped",0),
            "first_artist": first.get("artist",""),
            "first_album": first.get("album",""),
        })
        self._save_history(hist)

    # ---------- Queue flow ----------
    def set_running(self, running: bool):
        for w in [
            self.urls_edit, self.dest_edit, self.format_combo, self.parallel_spin,
            self.force_chk, self.extra_args, self.settings_btn, self.history_btn, self.toggle_terminal_btn
        ]:
            w.setEnabled(not running)
        self.run_btn.setEnabled(not running)
        self.stop_btn.setEnabled(running)
        self.run_btn.setText("Run" if not running else "Running…")
        self.pause_btn.setEnabled(running)   # pause control available only while queue is active
        # When starting, reset the label
        if running:
            self._paused = False
            self.pause_btn.setText("Pause after current")

    def start_queue(self):
        if self.proc_qt:
            QMessageBox.information(self, "Busy", "A run is already in progress.")
            return

        # Parse and validate URLs
        urls = [ln.strip().strip("'\"") for ln in self.urls_edit.toPlainText().splitlines() if ln.strip()]
        if not urls:
            QMessageBox.critical(self, "Cannot start", "Please paste at least one Spotify URL or URI.")
            return
        bad = [u for u in urls if not SPOTIFY_URL_RE.match(u)]
        if bad:
            proceed = QMessageBox.question(
                self, "Unusual input detected",
                "Some lines don’t look like Spotify links/URIs:\n- " + "\n- ".join(bad[:5]) + ("\n…" if len(bad) > 5 else "") +
                "\n\nContinue anyway?",
                QMessageBox.Yes | QMessageBox.No, QMessageBox.Yes
            )
            if proceed != QMessageBox.Yes:
                return

        self.save_main_fields()
        self._queue_urls = urls
        self._queue_index = 0
        self._queue_agg = {"moved":0, "replaced":0, "deleted":0, "skipped":0, "ok":0, "fail":0}
        self.set_running(True)

        if platform.system() == "Windows" and self.settings.value("persistent_terminal", "false") == "true":
            self.ensure_persistent_terminal(start_hidden=True)

        # instead of: self._start_next_job()
        self._schedule_next_job(0)


    def _build_base_args(self):
        dest = self.dest_edit.text().strip()
        fmt = self.format_combo.currentText()
        args = []
        if dest: args += ["--destination", dest]
        if fmt:  args += ["--format", fmt]
        args += ["--parallel", str(self.parallel_spin.value())]
        if self.force_chk.isChecked(): args += ["--force"]
        extra = self.extra_args.text().strip()
        if extra:
            args += shlex.split(extra)
        return args

    def _start_next_job(self):
        self._last_job_output_count = 0
        # Done?
        if self._queue_index >= len(self._queue_urls):
            self._finish_queue()
            return

        url = self._queue_urls[self._queue_index]
        self._current_url = url
        self._job_outputs = []
        self._job_stats = {"moved":0, "replaced":0, "deleted":0, "skipped":0}
        self._job_start_iso = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        self._log_buffer = []
        self._run_started_at = time.time()
        dest = self.dest_edit.text().strip()
        self._pre_run_files = self._list_audio_files(Path(dest)) if dest else set()

        try:
            exe = self.resolve_binary()
        except Exception as e:
            QMessageBox.critical(self, "Cannot start", str(e))
            self.stop_queue()
            return

        args = self._build_base_args() + [url]

        # Start QProcess for this URL
        self.proc_qt = QProcess(self)
        self.proc_qt.setProgram(exe)
        self.proc_qt.setArguments(args)
        self.proc_qt.setProcessChannelMode(QProcess.MergedChannels)
        self.proc_qt.readyRead.connect(self._on_ready_log)
        self.proc_qt.finished.connect(self._on_job_finished)
        self.proc_qt.errorOccurred.connect(self._on_job_error)
        self.proc_qt.start()

        # Mirror to persistent terminal if enabled
        if platform.system() == "Windows" and self.settings.value("persistent_terminal", "false") == "true":
            cmdline = " ".join([exe] + [shlex.quote(a) for a in args])
            self.send_to_persistent(cmdline)

    def _on_ready_log(self):
        try:
            chunk = self.proc_qt.readAll().data().decode(errors="replace")
            if chunk:
                self._log_buffer.append(chunk)
        except Exception:
            pass

    def _on_job_finished(self, code, status):
        self.proc_qt = None
        dest = self.dest_edit.text().strip()

        # Rearrange new files for this URL
        if int(code) == 0:
            try:
                self._rearrange_new_downloads(dest)
                self._queue_agg["ok"] += 1
                # reset backoff on success if desired
                if BACKOFF_RESET_ON_SUCCESS:
                    self._consecutive_failures = 0
                    self._backoff_index = 0
            except Exception:
                pass
        else:
            self._queue_agg["fail"] += 1
            self._consecutive_failures += 1
            # bump backoff index but clamp to sequence
            if self._backoff_index < len(BACKOFF_SEQUENCE_SECONDS) - 1:
                self._backoff_index += 1

        # Write per-URL log & history
        log_path = self._write_log_file(dest, self._queue_index + 1)
        self._append_history(int(code), dest, log_path)

        # Determine throttle delay before the NEXT job
        delay_sec = 0

        # If this URL produced many tracks, apply throttle
        if self._last_job_output_count >= THROTTLE_TRACKS_THRESHOLD:
            delay_sec = max(delay_sec, BACKOFF_SEQUENCE_SECONDS[0])  # at least 10s

        # If we failed, apply progressive backoff
        if int(code) != 0:
            delay_sec = max(delay_sec, BACKOFF_SEQUENCE_SECONDS[self._backoff_index])

        # Cleanup per-job state
        self._pre_run_files = None
        self._run_started_at = None
        self._log_buffer = []
        self._queue_index += 1

        # Schedule next URL (respect pause)
        self._schedule_next_job(delay_sec)


    def _on_job_error(self, err):
        # Treat as finished with failure; QProcess will also call finished soon,
        # but we guard against duplicate finalize by checking self.proc_qt.
        pass

    def stop_queue(self):
        if self.proc_qt and self.proc_qt.state() != QProcess.NotRunning:
            self.proc_qt.terminate()
            if not self.proc_qt.waitForFinished(2000):
                self.proc_qt.kill()
        self.proc_qt = None
        self.set_running(False)
        self._queue_urls = []
        self._queue_index = 0
        self._pre_run_files = None
        self._run_started_at = None
        self._log_buffer = []
        self._paused = False
        self.pause_btn.setText("Pause after current")
        self.pause_btn.setEnabled(False)


    def _finish_queue(self):
        self.set_running(False)
        totals = self._queue_agg
        msg = (
            f"All done.\n\n"
            f"Jobs OK: {totals['ok']}   |   Jobs Failed: {totals['fail']}\n"
            f"Moved: {totals['moved']}   Replaced: {totals['replaced']}   "
            f"Deleted: {totals['deleted']}   Skipped: {totals['skipped']}"
        )
        m = QMessageBox(self)
        m.setIcon(QMessageBox.Information)
        m.setWindowTitle("Queue complete")
        m.setText(msg)
        m.exec()

        # Optionally open destination if at least one job succeeded
        if totals["ok"] > 0 and self.settings.value("open_when_done", "false") == "true":
            dest = self.dest_edit.text().strip()
            if dest and Path(dest).exists():
                self.open_folder(dest)

    # ---------- UI helpers ----------
    def build_command_preview(self):  # (not shown; kept for potential future UI)
        try:
            exe = self.resolve_binary()
        except Exception:
            exe = "spotify-dl"
        args = self._build_base_args() + ["<url>"]
        return " ".join([exe] + [shlex.quote(a) for a in args])

    def on_error_qt(self, err):  # not used in queued flow; kept for completeness
        self.proc_qt = None
        self.set_running(False)
        m = QMessageBox(self)
        m.setIcon(QMessageBox.Critical)
        m.setWindowTitle("Failed to start")
        m.setText(f"Error: {err}")
        m.exec()

    def open_folder(self, folder: str):
        try:
            if sys.platform.startswith("win"):
                os.startfile(folder)  # type: ignore
            elif sys.platform == "darwin":
                subprocess.Popen(["open", folder])
            else:
                subprocess.Popen(["xdg-open", folder])
        except Exception:
            pass


if __name__ == "__main__":
    app = QApplication(sys.argv)
    app.setApplicationName(APP_NAME)
    app.setOrganizationName(APP_ORG)
    apply_dark_theme(app)
    w = SpotifyDLGui()
    w.show()
    sys.exit(app.exec())
