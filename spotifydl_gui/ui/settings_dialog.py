# spotifydl_gui/ui/settings_dialog.py
"""
Settings dialog (v0.9.5)

Groups:
- General (open when done, minimize to tray, persistent terminal)
- Download Organization (enable + folder template with live preview)
- Duplicates (keep larger, delete smaller)
- Integrity (size/duration thresholds)
- Cover Art (extract if missing)
- Playlists (Smart Sync: mirror playlist contents after runs)
- M3U8 Export (per-run playlist files)
- Runner (Adaptive parallelism)
- Scheduler (optional daily run time)
- Binary (custom spotify-dl path or auto-detect)

All settings are written to QSettings on OK.
"""

from __future__ import annotations

import platform
from pathlib import Path

from PySide6.QtCore import Qt
from PySide6.QtWidgets import (
    QDialog, QVBoxLayout, QHBoxLayout, QLabel, QLineEdit, QCheckBox, QSpinBox, QDoubleSpinBox,
    QPushButton, QFileDialog, QDialogButtonBox, QWidget, QMessageBox, QProgressDialog, QApplication,
    QScrollArea
)

from ..settings_store import get_settings, KEYS
from .. import organizer as org  # NEW: call reorganize_library(...)


class SettingsDialog(QDialog):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Settings")
        self.setMinimumWidth(720)
        self.s = get_settings()

        # ---------- General ----------
        self.open_when_done = QCheckBox("Open destination folder when done")
        self.minimize_to_tray = QCheckBox("Minimize to tray on close (keep running)")
        self.persistent_terminal = QCheckBox("Enable persistent terminal (Windows)")
        if platform.system() != "Windows":
            self.persistent_terminal.setEnabled(False)
            self.persistent_terminal.setToolTip("Windows only")

        # ---------- Organization ----------
        self.organize_enabled = QCheckBox("Rearrange downloaded files using folder template")
        self.template_edit = QLineEdit()
        self.template_edit.setPlaceholderText("{artist}/{album}")
        self.template_preview = QLabel("Preview: ")
        self.template_preview.setProperty("class", "muted")
        self.template_help = QLabel(
            "Tokens: {artist}, {album}, {title}, {track}, {disc}, {year}, {ext}, {filename}  "
            "(width: {track:02d})"
        )
        self.template_help.setProperty("class", "muted")
        self.template_edit.textChanged.connect(self._update_preview)
        # One-click reorganize button
        self.btn_organize_now = QPushButton("Organize destination now")
        self.btn_organize_now.setToolTip(
            "Sweep the current destination folder and apply your folder template, "
            "duplicate handling, cover extraction, and integrity checks."
        )
        self.btn_organize_now.clicked.connect(self._organize_now)
        self.btn_cleanup = QPushButton("Clean empty folders")
        self.btn_cleanup.setToolTip("Remove folders under the destination that no longer contain audio files.")
        self.btn_cleanup.clicked.connect(self._cleanup_destination)


        # ---------- Duplicates ----------
        self.dup_keep_larger = QCheckBox("Automatically keep the larger file when duplicates conflict")
        self.dup_delete_smaller = QCheckBox("Delete the smaller/equal file (otherwise skip)")

        # ---------- Integrity ----------
        self.integrity_flag = QCheckBox("Flag incomplete downloads")
        self.integrity_min_mb = QDoubleSpinBox(); self.integrity_min_mb.setRange(0.1, 100.0)
        self.integrity_min_mb.setSingleStep(0.1); self.integrity_min_mb.setSuffix(" MB")
        self.integrity_min_sec_flag = QCheckBox("Also flag when duration is under")
        self.integrity_min_sec = QSpinBox(); self.integrity_min_sec.setRange(1, 300); self.integrity_min_sec.setSuffix(" sec")

        # ---------- Cover ----------
        self.cover_extract = QCheckBox("Extract embedded cover art into folder if missing")

        # ---------- Playlists ----------
        self.smart_sync = QCheckBox("Smart Sync playlists (mirror playlist contents after run)")

        # ---------- M3U8 ----------
        self.m3u_export = QCheckBox("Export M3U8 playlist per run")
        self.m3u_in_one_folder = QCheckBox("If all files land in one folder, write M3U8 there (otherwise in _playlists/)")

        # ---------- History ----------
        self.history_max_lbl = QLabel("History capacity (entries)")
        self.history_max = QSpinBox(); self.history_max.setRange(10, 5000); self.history_max.setSingleStep(10)

        # ---------- Sentry Mode ----------
        self.sentry_enabled = QCheckBox("Enable Sentry mode (auto-download from clipboard while idle)")
        self.sentry_gap_sec = QSpinBox(); self.sentry_gap_sec.setRange(25, 600); self.sentry_gap_sec.setSuffix(" sec")
        self.sentry_hint = QLabel("When enabled, new Spotify links copied to clipboard are added and run automatically with a delay between jobs.")
        self.sentry_hint.setProperty("class", "muted")

        # ---------- Web server ----------
        self.web_enabled = QCheckBox("Enable web submission server")
        self.web_host = QLineEdit(); self.web_host.setPlaceholderText("127.0.0.1")
        self.web_port = QSpinBox(); self.web_port.setRange(1, 65535); self.web_port.setValue(9753)
        self.web_username = QLineEdit(); self.web_username.setPlaceholderText("Optional username")
        self.web_password = QLineEdit(); self.web_password.setEchoMode(QLineEdit.Password)
        self.web_password.setPlaceholderText("Optional password")
        self.web_dest = QLineEdit(); self.web_dest.setPlaceholderText("Media library destination override")
        self.web_dest_btn = QPushButton("Browse…")
        self.web_dest_btn.clicked.connect(self._browse_web_dest)

        # ---------- Runner ----------
        self.adaptive_parallel = QCheckBox("Adaptive parallelism (auto-adjust --parallel on failures/successes)")
        self.failure_delay_ms = QSpinBox(); self.failure_delay_ms.setRange(0, 600000); self.failure_delay_ms.setSingleStep(100)
        self.failure_delay_ms.setSuffix(" ms")
        self.failure_delay_multiplier = QDoubleSpinBox(); self.failure_delay_multiplier.setRange(1.0, 10.0); self.failure_delay_multiplier.setSingleStep(0.1)
        self.failure_delay_multiplier.setDecimals(2)
        self.failure_delay_multiplier.setSuffix(" ×")
        self.failure_delay_max_ms = QSpinBox(); self.failure_delay_max_ms.setRange(0, 600000); self.failure_delay_max_ms.setSingleStep(100)
        self.failure_delay_max_ms.setSuffix(" ms")

        # ---------- Scheduler ----------
        self.scheduler_enabled = QCheckBox("Enable daily scheduler")
        self.scheduler_time = QLineEdit()
        self.scheduler_time.setPlaceholderText("HH:MM (24h) — e.g. 02:30")
        self.scheduler_hint = QLabel("If enabled, the app will run queued URLs at this time (when idle).")
        self.scheduler_hint.setProperty("class", "muted")

        # ---------- Binary ----------
        self.bin_edit = QLineEdit()
        self.bin_edit.setPlaceholderText("Path to spotify-dl (optional; blank = auto-detect)")
        self.btn_bin = QPushButton("Browse…")
        self.btn_bin.clicked.connect(self._pick_bin)

        # ---------- Buttons ----------
        btns = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        btns.accepted.connect(self._accept)
        btns.rejected.connect(self.reject)

        # ---------- Layout ----------
        content = QWidget()
        layout = QVBoxLayout(content)

        def section(title: str) -> QLabel:
            lab = QLabel(title)
            lab.setStyleSheet("font-weight: 600; margin-top: 6px;")
            return lab

        # General
        layout.addWidget(section("General"))
        layout.addWidget(self.open_when_done)
        layout.addWidget(self.minimize_to_tray)
        layout.addWidget(self.persistent_terminal)

        # Organization
        layout.addWidget(section("Download Organization"))
        layout.addWidget(self.organize_enabled)
        layout.addWidget(self.template_edit)
        layout.addWidget(self.template_preview)
        layout.addWidget(self.template_help)
        layout.addLayout(_hbox([self.btn_organize_now, self.btn_cleanup], stretch_last=True))


        # Duplicates
        layout.addWidget(section("Duplicate Handling"))
        layout.addWidget(self.dup_keep_larger)
        layout.addWidget(self.dup_delete_smaller)

        # Integrity
        layout.addWidget(section("Integrity"))
        row1 = _hbox([self.integrity_flag])
        row2 = _hbox([QLabel("Size threshold:"), self.integrity_min_mb])
        row3 = _hbox([self.integrity_min_sec_flag, self.integrity_min_sec])
        layout.addLayout(row1); layout.addLayout(row2); layout.addLayout(row3)

        # Cover
        layout.addWidget(section("Cover Art"))
        layout.addWidget(self.cover_extract)

        # Playlists
        layout.addWidget(section("Playlists"))
        layout.addWidget(self.smart_sync)

        # M3U8
        layout.addWidget(section("M3U8 Export"))
        layout.addWidget(self.m3u_export)
        layout.addWidget(self.m3u_in_one_folder)

        # History
        layout.addWidget(section("History"))
        layout.addLayout(_hbox([self.history_max_lbl, self.history_max]))

        # Runner
        layout.addWidget(section("Runner"))
        layout.addWidget(self.adaptive_parallel)
        layout.addLayout(_hbox([QLabel("Failure delay base"), self.failure_delay_ms], stretch_last=True))
        layout.addLayout(_hbox([QLabel("Failure delay multiplier"), self.failure_delay_multiplier], stretch_last=True))
        layout.addLayout(_hbox([QLabel("Failure delay max"), self.failure_delay_max_ms], stretch_last=True))

        # Scheduler
        layout.addWidget(section("Scheduler"))
        layout.addWidget(self.scheduler_enabled)
        layout.addWidget(self.scheduler_time)
        layout.addWidget(self.scheduler_hint)

        # Sentry
        layout.addWidget(section("Sentry Mode"))
        row_s1 = _hbox([self.sentry_enabled])
        row_s2 = _hbox([QLabel("Gap between jobs:"), self.sentry_gap_sec])
        layout.addLayout(row_s1); layout.addLayout(row_s2); layout.addWidget(self.sentry_hint)

        # Web server
        layout.addWidget(section("Web Server"))
        layout.addWidget(self.web_enabled)
        layout.addLayout(_hbox([QLabel("Host"), self.web_host], stretch_last=True))
        layout.addLayout(_hbox([QLabel("Port"), self.web_port], stretch_last=True))
        layout.addLayout(_hbox([QLabel("Username"), self.web_username], stretch_last=True))
        layout.addLayout(_hbox([QLabel("Password"), self.web_password], stretch_last=True))
        layout.addLayout(_hbox([self.web_dest, self.web_dest_btn], stretch_last=True))

        # Binary
        layout.addWidget(section("spotify-dl Binary"))
        br = _hbox([self.bin_edit, self.btn_bin], stretch_last=True)
        layout.addLayout(br)


        scroll = QScrollArea(self)
        scroll.setWidgetResizable(True)
        scroll.setWidget(content)

        main_layout = QVBoxLayout(self)
        main_layout.addWidget(scroll, 1)
        main_layout.addWidget(btns)

        # Load current values
        self._load()

    # --------- internals ----------
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
            sub = (self.template_edit.text() or "{artist}/{album}").format_map(FmtDict(demo))
        except Exception:
            sub = "(invalid template)"
        preview = sub.replace("//", "/").strip("/\\") or "(root)"
        self.template_preview.setText(f"Preview: {preview}")

    def _pick_bin(self):
        path, _ = QFileDialog.getOpenFileName(self, "Select spotify-dl executable")
        if path:
            self.bin_edit.setText(path)

    def _load(self):
        v = self.s.value
        self.open_when_done.setChecked(str(v(KEYS["open_when_done"], "false")).lower() == "true")
        self.minimize_to_tray.setChecked(str(v(KEYS["minimize_to_tray"], "true")).lower() == "true")
        self.persistent_terminal.setChecked(str(v(KEYS["persistent_terminal"], "false")).lower() == "true")

        self.organize_enabled.setChecked(str(v(KEYS["organize_enabled"], "true")).lower() == "true")
        self.template_edit.setText(v(KEYS["template"], "{artist}/{album}"))

        self.dup_keep_larger.setChecked(str(v(KEYS["dup_resolve"], "true")).lower() == "true")
        self.dup_delete_smaller.setChecked(str(v(KEYS["dup_delete_smaller"], "true")).lower() == "true")

        self.integrity_flag.setChecked(str(v(KEYS["integrity_flag"], "true")).lower() == "true")
        try:
            self.integrity_min_mb.setValue(float(v(KEYS["integrity_min_mb"], 1.0)))
        except Exception:
            self.integrity_min_mb.setValue(1.0)
        self.integrity_min_sec_flag.setChecked(str(v(KEYS["integrity_duration_flag"], "false")).lower() == "true")
        try:
            self.integrity_min_sec.setValue(int(v(KEYS["integrity_min_seconds"], 10)))
        except Exception:
            self.integrity_min_sec.setValue(10)

        self.cover_extract.setChecked(str(v(KEYS["cover_extract"], "true")).lower() == "true")

        self.smart_sync.setChecked(str(v(KEYS["m3u_in_folder_when_single"], "true")).lower() == "true")  # note: separate toggle below
        # For Smart Sync we use a new key. If missing, default ON:
        self.smart_sync.setChecked(str(v(KEYS.get("smart_sync", "smart_sync"), "true")).lower() == "true")

        self.m3u_export.setChecked(str(v(KEYS["m3u_export"], "true")).lower() == "true")
        self.m3u_in_one_folder.setChecked(str(v(KEYS["m3u_in_folder_when_single"], "true")).lower() == "true")

        try:
            self.history_max.setValue(int(v(KEYS.get("history_max", "history_max"), 100)))
        except Exception:
            self.history_max.setValue(100)

        self.adaptive_parallel.setChecked(str(v(KEYS["adaptive_parallel"], "true")).lower() == "true")
        try:
            self.failure_delay_ms.setValue(int(v(KEYS.get("failure_delay_ms", "failure_delay_ms"), 2000)))
        except Exception:
            self.failure_delay_ms.setValue(2000)
        try:
            self.failure_delay_multiplier.setValue(float(v(KEYS.get("failure_delay_multiplier", "failure_delay_multiplier"), 2.0)))
        except Exception:
            self.failure_delay_multiplier.setValue(2.0)
        try:
            self.failure_delay_max_ms.setValue(int(v(KEYS.get("failure_delay_max_ms", "failure_delay_max_ms"), 60000)))
        except Exception:
            self.failure_delay_max_ms.setValue(60000)

        self.sentry_enabled.setChecked(str(v(KEYS["sentry_enabled"], "false")).lower() == "true")
        try:
            self.sentry_gap_sec.setValue(max(25, int(v(KEYS["sentry_gap_sec"], 25))))
        except Exception:
            self.sentry_gap_sec.setValue(25)

        self.web_enabled.setChecked(str(v(KEYS.get("web_enabled", "web_enabled"), "false")).lower() == "true")
        self.web_host.setText(str(v(KEYS.get("web_host", "web_host"), "127.0.0.1")))
        try:
            self.web_port.setValue(int(v(KEYS.get("web_port", "web_port"), 9753)))
        except Exception:
            self.web_port.setValue(9753)
        self.web_username.setText(str(v(KEYS.get("web_username", "web_username"), "")))
        self.web_password.setText(str(v(KEYS.get("web_password", "web_password"), "")))
        self.web_dest.setText(str(v(KEYS.get("web_dest_override", "web_dest_override"), "")))

        self.scheduler_enabled.setChecked(str(v(KEYS["scheduler_enabled"], "false")).lower() == "true")
        self.scheduler_time.setText(str(v(KEYS["scheduler_time"], "")))

        self.bin_edit.setText(str(v(KEYS["bin"], "")))

        self._update_preview()

    def _accept(self):
        s = self.s
        s.setValue(KEYS["open_when_done"], "true" if self.open_when_done.isChecked() else "false")
        s.setValue(KEYS["minimize_to_tray"], "true" if self.minimize_to_tray.isChecked() else "false")
        s.setValue(KEYS["persistent_terminal"], "true" if self.persistent_terminal.isChecked() else "false")

        s.setValue(KEYS["organize_enabled"], "true" if self.organize_enabled.isChecked() else "false")
        s.setValue(KEYS["template"], self.template_edit.text().strip() or "{artist}/{album}")

        s.setValue(KEYS["dup_resolve"], "true" if self.dup_keep_larger.isChecked() else "false")
        s.setValue(KEYS["dup_delete_smaller"], "true" if self.dup_delete_smaller.isChecked() else "false")

        s.setValue(KEYS["integrity_flag"], "true" if self.integrity_flag.isChecked() else "false")
        s.setValue(KEYS["integrity_min_mb"], str(self.integrity_min_mb.value()))
        s.setValue(KEYS["integrity_duration_flag"], "true" if self.integrity_min_sec_flag.isChecked() else "false")
        s.setValue(KEYS["integrity_min_seconds"], str(self.integrity_min_sec.value()))

        s.setValue(KEYS["cover_extract"], "true" if self.cover_extract.isChecked() else "false")

        s.setValue(KEYS.get("smart_sync", "smart_sync"), "true" if self.smart_sync.isChecked() else "false")

        s.setValue(KEYS["m3u_export"], "true" if self.m3u_export.isChecked() else "false")
        s.setValue(KEYS["m3u_in_folder_when_single"], "true" if self.m3u_in_one_folder.isChecked() else "false")

        s.setValue(KEYS.get("history_max", "history_max"), str(self.history_max.value()))

        s.setValue(KEYS["sentry_enabled"], "true" if self.sentry_enabled.isChecked() else "false")
        s.setValue(KEYS["sentry_gap_sec"], str(max(25, self.sentry_gap_sec.value())))

        s.setValue(KEYS.get("web_enabled", "web_enabled"), "true" if self.web_enabled.isChecked() else "false")
        s.setValue(KEYS.get("web_host", "web_host"), self.web_host.text().strip() or "127.0.0.1")
        s.setValue(KEYS.get("web_port", "web_port"), str(self.web_port.value()))
        s.setValue(KEYS.get("web_username", "web_username"), self.web_username.text().strip())
        s.setValue(KEYS.get("web_password", "web_password"), self.web_password.text())
        s.setValue(KEYS.get("web_dest_override", "web_dest_override"), self.web_dest.text().strip())

        s.setValue(KEYS["adaptive_parallel"], "true" if self.adaptive_parallel.isChecked() else "false")
        s.setValue(KEYS.get("failure_delay_ms", "failure_delay_ms"), str(self.failure_delay_ms.value()))
        s.setValue(KEYS.get("failure_delay_multiplier", "failure_delay_multiplier"), str(self.failure_delay_multiplier.value()))
        s.setValue(KEYS.get("failure_delay_max_ms", "failure_delay_max_ms"), str(self.failure_delay_max_ms.value()))

        s.setValue(KEYS["scheduler_enabled"], "true" if self.scheduler_enabled.isChecked() else "false")
        s.setValue(KEYS["scheduler_time"], self.scheduler_time.text().strip())

        s.setValue(KEYS["bin"], self.bin_edit.text().strip())

        try:
            s.sync()
        except Exception:
            pass

        parent = self.parent()
        if parent and hasattr(parent, '_configure_web_server'):
            try:
                parent._configure_web_server(force=True)
            except Exception:
                pass

        self.accept()

    # -------- Destination helpers --------
    def _cleanup_destination(self) -> None:
        dest = str(self.s.value("dest", "") or "").strip()
        if not dest or not Path(dest).exists():
            QMessageBox.critical(
                self,
                "Destination missing",
                "Set a valid destination folder on the main screen before cleaning.",
            )
            return

        if QMessageBox.question(
            self,
            "Clean destination",
            "Delete empty folders (and leftover cover/info files) inside the destination?\n\nThis cannot be undone.",
            QMessageBox.Yes | QMessageBox.No,
            QMessageBox.No,
        ) != QMessageBox.Yes:
            return

        prog = QProgressDialog("Cleaning…", "Cancel", 0, 0, self)
        prog.setWindowModality(Qt.ApplicationModal)
        prog.setMinimumDuration(0)
        prog.show()
        QApplication.processEvents()

        try:
            removed = org.cleanup_empty_folders(dest)
        except Exception as e:
            prog.close()
            QMessageBox.critical(self, "Cleanup failed", str(e))
            return
        finally:
            prog.close()

        QMessageBox.information(
            self,
            "Cleanup complete",
            f"Removed {removed} folder(s).",
        )

    # -------- Organize destination now --------
    def _organize_now(self):
        dest = str(self.s.value("dest", "") or "").strip()
        if not dest:
            QMessageBox.critical(self, "Destination missing",
                                 "Set a destination folder on the main screen before organizing.")
            return
        if not Path(dest).exists():
            QMessageBox.critical(self, "Folder not found",
                                 f"The folder does not exist:\n{dest}")
            return

        # Confirm
        if QMessageBox.question(
            self, "Organize destination",
            "This will reorganize ALL audio files in the destination using your current settings.\n\nProceed?",
            QMessageBox.Yes | QMessageBox.No, QMessageBox.Yes
        ) != QMessageBox.Yes:
            return

        # Busy/progress (indeterminate)
        prog = QProgressDialog("Organizing…", "Cancel", 0, 0, self)
        prog.setWindowModality(Qt.ApplicationModal)
        prog.setMinimumDuration(0)
        prog.show()
        QApplication.processEvents()

        try:
            outputs, suspects, stats = org.reorganize_library(dest, self.s)
        except Exception as e:
            prog.close()
            QMessageBox.critical(self, "Organize failed", str(e))
            return
        finally:
            prog.close()

        moved = stats.get("moved", 0)
        replaced = stats.get("replaced", 0)
        deleted = stats.get("deleted", 0)
        skipped = stats.get("skipped", 0)
        sus = len(suspects or [])

        QMessageBox.information(
            self, "Organize complete",
            f"Moved: {moved}\nReplaced: {replaced}\nDeleted: {deleted}\nSkipped: {skipped}\n"
            f"Suspect files: {sus}"
        )



# ---------- helpers ----------
    def _browse_web_dest(self):
        folder = QFileDialog.getExistingDirectory(self, "Select media destination override")
        if folder:
            self.web_dest.setText(folder)

def _hbox(widgets: list[QWidget], stretch_last: bool = False):
    row = QHBoxLayout()
    for i, w in enumerate(widgets):
        row.addWidget(w)
        if stretch_last and i == len(widgets) - 1:
            row.addStretch()
    return row
