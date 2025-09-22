# spotifydl_gui/ui/main_window.py
"""
MainWindow — wires UI + Runner + Settings + History for spotify-dl GUI v0.8

Features:
- Queue panel with per-row progress bars (QueueRow)
- Add/remove/clear/reorder, de-dupe, drag-drop text
- Import/Export queue (JSON or TXT)
- Run / Pause / Stop controls
- Live raw-output tail (right side)
- Backoff banner + adaptive-parallel indicator
- Tray icon + minimize-to-tray (respects setting)
- Settings & History dialogs
- Clipboard watcher: auto-add unique Spotify links
- Scheduler (daily at HH:MM)
- Persistent terminal (Windows, optional)
- Sentry mode: background clipboard capture with history de-dupe + auto-run + fixed inter-job gap
"""

from __future__ import annotations

import os
import sys
import re
import json
import time
import shlex
import platform
import subprocess
from pathlib import Path
from typing import List

from PySide6.QtCore import Qt, QTimer, QTime, QUrl
from PySide6.QtGui import QAction, QIcon, QDesktopServices, QTextCursor
from PySide6.QtWidgets import (
    QWidget, QHBoxLayout, QVBoxLayout, QLabel, QPushButton, QTextEdit, QLineEdit, QComboBox,
    QSpinBox, QListWidget, QListWidgetItem, QFileDialog, QMessageBox, QMenu, QSystemTrayIcon,
    QCheckBox, QApplication
)

from ..settings_store import get_settings, KEYS, APP_NAME, APP_VER
from ..runner import Runner, RunOptions, SPOTIFY_URL_RE
from ..utils import get_app_icon, console_hwnd_for_pid, show_window, resolve_spotifydl_binary
from .queue_row import QueueRow, QStatus
from .settings_dialog import SettingsDialog
from .history_dialog import HistoryDialog
from .shortcuts_dialog import ShortcutsDialog


CLIPBOARD_POLL_MS = 1000
PERCENT_RE = re.compile(r"(?<!\d)(\d{1,3})%(?!\d)")


class MainWindow(QWidget):
    def __init__(self, app_icon: QIcon | None = None):
        super().__init__()
        self.s = get_settings()
        self.setWindowTitle(f"{APP_NAME} • {APP_VER}")
        self.setMinimumSize(1120, 760)
        if app_icon:
            self.setWindowIcon(app_icon)
        self._really_quit = False  # set True when user picks Quit explicitly

        # ---- Header ----
        title = QLabel(f"spotify-dl — simple GUI  •  {APP_VER}")
        title.setStyleSheet("font-size: 18px; font-weight: 600;")
        self.btn_history = QPushButton("History"); self.btn_history.clicked.connect(self.open_history)
        self.btn_shortcuts = QPushButton("Shortcuts"); self.btn_shortcuts.clicked.connect(self.open_shortcuts)
        self.btn_settings = QPushButton("Settings"); self.btn_settings.clicked.connect(self.open_settings)
        self.btn_quit = QPushButton("Quit")
        self.btn_quit.clicked.connect(self._quit)

        hdr = QHBoxLayout()
        hdr.addWidget(title)
        hdr.addStretch()
        hdr.addWidget(self.btn_history)
        hdr.addWidget(self.btn_shortcuts)
        hdr.addWidget(self.btn_settings)
        hdr.addWidget(self.btn_quit)

        # ---- Left: Queue panel ----
        self.queue = QListWidget()
        self.queue.setSelectionMode(QListWidget.ExtendedSelection)
        self.queue.setSpacing(6)
        self.queue.setDragEnabled(True)
        self.queue.setAcceptDrops(True)
        self.queue.setDragDropMode(QListWidget.InternalMove)
        self.queue.setDefaultDropAction(Qt.MoveAction)
        self.queue.setContextMenuPolicy(Qt.CustomContextMenu)
        self.queue.customContextMenuRequested.connect(self._queue_context_menu)
        # Persist order on drag-reorder
        try:
            self.queue.model().rowsMoved.connect(lambda *args: self._save_queue_state())
        except Exception:
            pass

        self.btn_add = QPushButton("Add URLs ➜ Queue"); self.btn_add.clicked.connect(self._add_from_staging)
        self.btn_remove = QPushButton("Remove"); self.btn_remove.clicked.connect(self._remove_selected)
        self.btn_clear = QPushButton("Clear"); self.btn_clear.clicked.connect(self._clear_queue)
        self.btn_up = QPushButton("↑"); self.btn_up.clicked.connect(lambda: self._nudge(-1))
        self.btn_down = QPushButton("↓"); self.btn_down.clicked.connect(lambda: self._nudge(+1))

        self.btn_run = QPushButton("Run"); self.btn_run.clicked.connect(self._run)
        self.btn_pause = QPushButton("Pause after current"); self.btn_pause.setEnabled(False)
        self.btn_pause.clicked.connect(self._pause_toggle)
        self.btn_stop = QPushButton("Stop"); self.btn_stop.setEnabled(False)
        self.btn_stop.clicked.connect(self._stop)

        self.btn_import = QPushButton("Import queue…"); self.btn_import.clicked.connect(self._import_queue)
        self.btn_export = QPushButton("Export queue…"); self.btn_export.clicked.connect(self._export_queue)

        self.auto_clip = QCheckBox("Auto-add clipboard links"); self.auto_clip.setChecked(True)

        ltop = QHBoxLayout()
        ltop.addWidget(self.btn_add); ltop.addStretch()
        ltop.addWidget(self.btn_up); ltop.addWidget(self.btn_down)
        ltop.addSpacing(8); ltop.addWidget(self.btn_remove); ltop.addWidget(self.btn_clear)

        # Extra queue actions: Retry failed / Remove completed
        self.btn_retry_failed = QPushButton("Retry failed"); self.btn_retry_failed.clicked.connect(self._retry_failed)
        self.btn_remove_completed = QPushButton("Remove completed"); self.btn_remove_completed.clicked.connect(self._remove_completed)

        lrun = QHBoxLayout()
        lrun.addWidget(self.btn_run); lrun.addWidget(self.btn_pause); lrun.addWidget(self.btn_stop)
        lrun.addSpacing(8)
        lrun.addWidget(self.btn_retry_failed); lrun.addWidget(self.btn_remove_completed)
        lrun.addStretch(); lrun.addWidget(self.btn_import); lrun.addWidget(self.btn_export)

        left = QVBoxLayout()
        left.addLayout(ltop)
        left.addWidget(self.queue, 1)
        left.addSpacing(6)
        left.addWidget(self.auto_clip)
        left.addLayout(lrun)

        # ---- Right: Staging + options + live tail ----
        self.staging = QTextEdit()
        self.staging.setAcceptRichText(False)
        self.staging.setPlaceholderText("Paste Spotify URLs here (one per line), then click “Add URLs ➜ Queue”")

        self.dest = QLineEdit(); self.dest.setPlaceholderText("Choose destination folder")
        self.btn_dest = QPushButton("Browse…"); self.btn_dest.clicked.connect(self._pick_dest)

        dr = QHBoxLayout()
        dr.addWidget(QLabel("Destination")); dr.addSpacing(6)
        dr.addWidget(self.dest, 1); dr.addWidget(self.btn_dest)

        self.format = QComboBox(); self.format.addItems(["flac", "mp3", "m4a", "opus"])
        self.parallel = QSpinBox(); self.parallel.setRange(1, 32); self.parallel.setValue(5)
        self.force = QCheckBox("Force re-download")
        orow = QHBoxLayout()
        orow.addWidget(QLabel("Format")); orow.addWidget(self.format)
        orow.addSpacing(16); orow.addWidget(QLabel("Parallel")); orow.addWidget(self.parallel)
        orow.addSpacing(16); orow.addWidget(self.force)
        orow.addStretch()

        self.extra = QLineEdit(); self.extra.setPlaceholderText("Optional extra flags (advanced)")

        self.backoff_label = QLabel(""); self.backoff_label.setProperty("class", "muted")
        self.adapt_label = QLabel(""); self.adapt_label.setProperty("class", "muted")
        self.time_label = QLabel(""); self.time_label.setProperty("class", "muted")

        util = QHBoxLayout()
        util.addWidget(self.adapt_label)
        util.addSpacing(8)
        util.addWidget(self.time_label)
        util.addStretch()
        util.addWidget(self.backoff_label)

        self.tail = QTextEdit(); self.tail.setReadOnly(True); self.tail.setVisible(False)

        disclaimer = QLabel("Requires Spotify Premium. Use at your own risk — may violate Spotify Terms or local laws.")
        disclaimer.setWordWrap(True); disclaimer.setProperty("class", "muted")

        # Footer: Sentry indicator + binary pill
        self.bin_pill = QLabel(); self.bin_pill.setObjectName("binPill")
        self._sentry_label = QLabel(""); self._sentry_label.setProperty("class", "muted")
        right_footer = QHBoxLayout()
        right_footer.addWidget(self._sentry_label)
        right_footer.addStretch()
        right_footer.addWidget(self.bin_pill)

        right = QVBoxLayout()
        right.addLayout(hdr); right.addSpacing(4)
        right.addWidget(self._hline()); right.addSpacing(4)
        right.addWidget(QLabel("Tracks / Playlists / Albums"))
        right.addWidget(self.staging)
        right.addLayout(dr)
        right.addSpacing(4)
        right.addLayout(orow)
        right.addSpacing(6)
        right.addWidget(QLabel("Extra flags"))
        right.addWidget(self.extra)
        right.addSpacing(6)
        right.addWidget(self._hline())
        right.addLayout(util)
        right.addSpacing(6)
        right.addWidget(self.tail, 1)
        right.addSpacing(6)
        right.addWidget(disclaimer)
        right.addSpacing(6)
        right.addLayout(right_footer)

        # Overall split
        lay = QHBoxLayout(self)
        lay.addLayout(left, 0); lay.addSpacing(10); lay.addLayout(right, 1)

        # ---- Tray ----
        self.tray: QSystemTrayIcon | None = None
        if QSystemTrayIcon.isSystemTrayAvailable():
            self.tray = QSystemTrayIcon(self)
            self.tray.setIcon(get_app_icon())
            menu = QMenu()
            act_show = QAction("Show", self)
            act_show.triggered.connect(self._tray_show)
            act_quit = QAction("Quit", self)
            act_quit.setShortcut("Ctrl+Q")
            act_quit.triggered.connect(self._quit)

            # Sentry toggle in tray
            self._sentry_enabled = self._read_bool(KEYS.get("sentry_enabled", "sentry_enabled"), False)
            try:
                self._sentry_gap_sec = max(25, int(self.s.value(KEYS.get("sentry_gap_sec", "sentry_gap_sec"), 25)))
            except Exception:
                self._sentry_gap_sec = 25
            act_sentry = QAction("Sentry mode", self); act_sentry.setCheckable(True)
            act_sentry.setChecked(self._sentry_enabled)
            act_sentry.toggled.connect(self._toggle_sentry)

            menu.addAction(act_sentry)
            menu.addSeparator()
            menu.addAction(act_show)
            menu.addSeparator()
            menu.addAction(act_quit)

            self.tray.setContextMenu(menu)
            self.tray.activated.connect(lambda reason: self._tray_show() if reason == QSystemTrayIcon.Trigger else None)
            self.tray.show()
        else:
            # Fallback Sentry defaults without tray
            self._sentry_enabled = self._read_bool(KEYS.get("sentry_enabled", "sentry_enabled"), False)
            try:
                self._sentry_gap_sec = max(25, int(self.s.value(KEYS.get("sentry_gap_sec", "sentry_gap_sec"), 25)))
            except Exception:
                self._sentry_gap_sec = 25

        # ---- Runner ----
        self.runner = Runner(self.s, self)
        self.runner.sig_job_started.connect(self._on_job_started)
        self.runner.sig_job_log.connect(self._on_job_log)
        self.runner.sig_job_progress.connect(self._on_job_progress)
        self.runner.sig_job_finished.connect(self._on_job_finished)
        self.runner.sig_queue_finished.connect(self._on_queue_finished)
        self.runner.sig_backoff_updated.connect(self._on_backoff_updated)
        self.runner.sig_command_line.connect(self._on_command_line)
        self.runner.sig_parallel_changed.connect(self._on_parallel_changed)
        self.runner.sig_rate_limit_notice.connect(self._on_rate_limit_notice)

        # ---- Clipboard watcher ----
        self._last_clip = ""
        self._clip_timer = QTimer(self); self._clip_timer.setInterval(CLIPBOARD_POLL_MS)
        self._clip_timer.timeout.connect(self._tick_clipboard); self._clip_timer.start()

        # ---- Scheduler ----
        self._sched_timer = QTimer(self); self._sched_timer.setInterval(30_000)  # twice a minute
        self._sched_timer.timeout.connect(self._tick_scheduler); self._sched_timer.start()

        # ---- Persistent terminal (Windows) ----
        self._persist_proc: subprocess.Popen | None = None
        self._persist_hwnd = None
        if platform.system() == "Windows" and self._read_bool(KEYS["persistent_terminal"], False):
            self._ensure_persistent_terminal(start_hidden=True)

        # ---- Load saved fields ----
        self._load_form()
        self._update_bin_pill()
        self._update_sentry_indicator()
        # Restore previous queue, if any
        try:
            self._restore_queue_state()
        except Exception:
            pass
        # Install global keyboard shortcuts
        self._install_shortcuts()
        # Runtime tracking for progress/ETA and Windows taskbar
        self._taskbar_btn = None
        self._taskbar_prog = None
        self._last_cmd = ""
        self._current_job_started_ts = None
        self._current_job_total = 0
        self._current_job_index = -1

    # --------------- UI helpers ---------------
    def _hline(self):
        from PySide6.QtWidgets import QFrame
        ln = QFrame(); ln.setFrameShape(QFrame.HLine); ln.setFrameShadow(QFrame.Sunken)
        ln.setStyleSheet("color:#2a2f39;")
        return ln

    def _notify(self, title: str, body: str, ms: int = 4000):
        if self.tray:
            self.tray.showMessage(title, body, QSystemTrayIcon.Information, ms)

    def _read_bool(self, key: str, default: bool) -> bool:
        return str(self.s.value(key, "true" if default else "false")).lower() == "true"

    def _read_int(self, key: str, default: int) -> int:
        try:
            return int(self.s.value(key, default))
        except Exception:
            return default

    def _read_float(self, key: str, default: float) -> float:
        try:
            return float(self.s.value(key, default))
        except Exception:
            return default

    def _load_form(self):
        self.dest.setText(self.s.value(KEYS["dest"], ""))
        self.format.setCurrentText(self.s.value(KEYS["format"], "flac"))
        try:
            self.parallel.setValue(int(self.s.value(KEYS["parallel"], 5)))
        except Exception:
            self.parallel.setValue(5)
        self.force.setChecked(self._read_bool(KEYS["force"], False))
        self.extra.setText(self.s.value(KEYS["extra"], ""))

    def _save_form(self):
        self.s.setValue(KEYS["dest"], self.dest.text().strip())
        self.s.setValue(KEYS["format"], self.format.currentText())
        self.s.setValue(KEYS["parallel"], self.parallel.value())
        self.s.setValue(KEYS["force"], "true" if self.force.isChecked() else "false")
        self.s.setValue(KEYS["extra"], self.extra.text().strip())

    def _update_bin_pill(self):
        try:
            path = resolve_spotifydl_binary(self.s)
            ver = self._get_spotifydl_version(path)
            ver_txt = f" ({ver})" if ver else ""
            text = f"Binary: {Path(path).name}{ver_txt}"
            tip = path if not ver else f"{path}\nVersion: {ver}"
            bg = "#1b2a22"; border = "#2a2f39"; color = "#e6eaf2"
        except Exception:
            text = "Binary: Not found"
            tip = "Place 'spotify-dl(.exe)' next to the app, set in Settings, or add to PATH."
            bg = "#2a1d1d"; border = "#f7768e"; color = "#fbcaca"
        self.bin_pill.setToolTip(tip)
        self.bin_pill.setText(text)
        self.bin_pill.setStyleSheet(f"""
            QLabel#binPill {{
                background: {bg};
                color: {color};
                border: 1px solid {border};
                border-radius: 999px;
                padding: 6px 10px;
            }}
        """)

    def _get_spotifydl_version(self, exe_path: str) -> str | None:
        try:
            import subprocess
            out = subprocess.run([exe_path, "--version"], capture_output=True, text=True, timeout=2)
            txt = (out.stdout or out.stderr or "").strip()
            return txt.splitlines()[0].strip() if txt else None
        except Exception:
            return None

    def _update_sentry_indicator(self):
        if getattr(self, '_sentry_enabled', False):
            gap = max(25, int(getattr(self, '_sentry_gap_sec', 25)))
            self._sentry_label.setText(f'Sentry: ON  -  gap {gap}s')
        else:
            self._sentry_label.setText('Sentry: OFF')

    def _quit(self):
        # If a run is in progress, confirm stopping
        if self.runner.is_running():
            resp = QMessageBox.question(
                self, "Quit",
                "A download is in progress. Quit and stop it?",
                QMessageBox.Yes | QMessageBox.No,
                QMessageBox.No
            )
            if resp != QMessageBox.Yes:
                return
            try:
                self.runner.stop()
            except Exception:
                pass
        self._really_quit = True
        self.close()  # will be accepted by closeEvent


    # --------------- Queue ops ---------------
    def _add_urls(self, urls: List[str], dedupe_history: bool = False):
        existing = {self._row(i).url() for i in range(self.queue.count())}
        # history set (successful jobs only)
        hist_inputs = set()
        if dedupe_history:
            try:
                for h in self._load_history():
                    if int(h.get("code", -1)) == 0 and h.get("input"):
                        hist_inputs.add(h["input"])
            except Exception:
                pass
        added = 0
        for u in urls:
            if not SPOTIFY_URL_RE.match(u):
                continue
            if u in existing:
                continue
            if dedupe_history and u in hist_inputs:
                continue
            roww = QueueRow(u, QStatus.PENDING)
            item = QListWidgetItem(self.queue)
            item.setSizeHint(roww.sizeHint())
            self.queue.addItem(item)
            self.queue.setItemWidget(item, roww)
            existing.add(u); added += 1
        if added:
            self._notify("Queued links", f"Added {added} link(s).", 2500)
            self._save_queue_state()

    def _add_from_staging(self):
        urls = [ln.strip() for ln in self.staging.toPlainText().splitlines() if ln.strip()]
        self._add_urls(urls, dedupe_history=False)
        self.staging.clear()

    def _row(self, i: int) -> QueueRow:
        it = self.queue.item(i)
        return self.queue.itemWidget(it)  # type: ignore

    def _selected_rows(self) -> list[int]:
        return sorted([self.queue.row(i) for i in self.queue.selectedItems()])

    def _remove_selected(self):
        for i in reversed(self._selected_rows()):
            self.queue.takeItem(i)
        self._save_queue_state()

    def _nudge(self, delta: int):
        idxs = self._selected_rows()
        if not idxs: return
        for idx in (idxs if delta > 0 else reversed(idxs)):
            new = idx + delta
            if new < 0 or new >= self.queue.count(): continue
            it = self.queue.takeItem(idx)
            self.queue.insertItem(new, it)
            self.queue.setItemSelected(it, True)
        self._save_queue_state()

        # --------------- Context menu ---------------
    def _queue_context_menu(self, pos):
        menu = QMenu(self)
        act_copy = QAction("Copy URL", self); act_copy.triggered.connect(self._ctx_copy_url)
        act_remove = QAction("Remove", self); act_remove.triggered.connect(self._remove_selected)
        act_open_sp = QAction("Open in Spotify", self); act_open_sp.triggered.connect(self._ctx_open_in_spotify)
        act_up = QAction("Move up", self); act_up.triggered.connect(lambda: self._nudge(-1))
        act_down = QAction("Move down", self); act_down.triggered.connect(lambda: self._nudge(+1))
        menu.addAction(act_copy)
        menu.addAction(act_open_sp)
        menu.addSeparator()
        menu.addAction(act_up); menu.addAction(act_down)
        menu.addSeparator()
        menu.addAction(act_remove)
        menu.exec(self.queue.mapToGlobal(pos))

    def _ctx_copy_url(self):
        rows = self._selected_rows()
        if not rows:
            return
        urls = [self._row(i).url() for i in rows]
        QApplication.clipboard().setText("\n".join(urls))

    def _ctx_open_in_spotify(self):
        rows = self._selected_rows()
        if not rows:
            return
        url = self._row(rows[0]).url()
        try:
            QDesktopServices.openUrl(QUrl(url))
        except Exception:
            pass

    # --------------- Shortcuts ---------------
    def _install_shortcuts(self):
        from PySide6.QtGui import QAction, QKeySequence

        specs = [
            ("Ctrl+R", "Run queue", self._run),
            ("Ctrl+P", "Pause/Resume queue", self._pause_toggle),
            ("Ctrl+S", "Stop", self._stop),
            ("Delete", "Remove selected from queue", self._remove_selected),
            ("Ctrl+I", "Import queue", self._import_queue),
            ("Ctrl+E", "Export queue", self._export_queue),
            ("Ctrl+L", "Focus links area", lambda: self.staging.setFocus()),
            ("Ctrl+Return", "Add URLs from links area", self._add_from_staging),
            ("Ctrl+D", "Focus destination field", lambda: self.dest.setFocus()),
            ("Ctrl+B", "Browse destination folder", self._pick_dest),
            ("Alt+Up", "Move selected up", lambda: self._nudge(-1)),
            ("Alt+Down", "Move selected down", lambda: self._nudge(+1)),
            ("Ctrl+H", "Open history", self.open_history),
            ("Ctrl+,", "Open settings", self.open_settings),
            ("F1", "Show shortcuts", self.open_shortcuts),
            ("Ctrl+Q", "Quit", self._quit),
        ]

        self._actions: list[QAction] = []
        for keys, _desc, slot in specs:
            act = QAction(self)
            act.setShortcut(QKeySequence(keys))
            act.triggered.connect(slot)
            act.setShortcutVisibleInContextMenu(False)
            self.addAction(act)
            self._actions.append(act)

        # Save list for dialog rendering
        self._shortcuts_list = [(k, d) for k, d, _ in specs]

    def open_shortcuts(self):
        entries = getattr(self, "_shortcuts_list", [])
        dlg = ShortcutsDialog(self, entries)
        dlg.exec()


    # --------------- Import / Export queue ---------------
    def _import_queue(self):
        path, _ = QFileDialog.getOpenFileName(self, "Import queue", "", "Queue files (*.json *.txt);;All files (*)")
        if not path:
            return
        try:
            text = Path(path).read_text(encoding="utf-8", errors="ignore")
            if path.lower().endswith(".json"):
                data = json.loads(text)
                urls = data.get("urls", []) if isinstance(data, dict) else data
                if not isinstance(urls, list):
                    raise ValueError('Invalid JSON format: expected {"urls": [...]} or a JSON list.')
            else:
                urls = [ln.strip() for ln in text.splitlines() if ln.strip()]
            self._add_urls(urls)
            self._notify("Queue imported", f"Added {len(urls)} link(s).", 2500)
        except Exception as e:
            QMessageBox.critical(self, "Import failed", str(e))

    def _export_queue(self):
        if self.queue.count() == 0:
            QMessageBox.information(self, "Nothing to export", "Queue is empty.")
            return
        path, _ = QFileDialog.getSaveFileName(self, "Export queue", "queue.json",
                                             "JSON (*.json);;Text (*.txt)")
        if not path:
            return
        urls = [self._row(i).url() for i in range(self.queue.count())]
        try:
            if path.lower().endswith(".json"):
                Path(path).write_text(json.dumps({"urls": urls}, indent=2), encoding="utf-8")
            else:
                Path(path).write_text("\n".join(urls), encoding="utf-8")
            self._notify("Queue exported", f"{len(urls)} link(s) saved.", 2500)
        except Exception as e:
            QMessageBox.critical(self, "Export failed", str(e))

    # --------------- File pickers ---------------
    def _pick_dest(self):
        p = QFileDialog.getExistingDirectory(self, "Choose destination folder")
        if p:
            self.dest.setText(p)

    # --------------- Runner control ---------------
    def _run(self):
        if self.runner.is_running():
            QMessageBox.information(self, "Busy", "A run is already in progress.")
            return
        # Add staged
        if self.staging.toPlainText().strip():
            self._add_from_staging()
        urls = self._pending_urls()

        if not urls:
            QMessageBox.information(self, "Nothing to do", "All items in the queue have already been processed.")
            return
        dest = self.dest.text().strip()
        if not dest:
            QMessageBox.critical(self, "Destination missing", "Choose a destination folder.")
            return
        try:
            Path(dest).mkdir(parents=True, exist_ok=True)
            t = Path(dest) / ".write_test.tmp"
            t.write_text("ok", encoding="utf-8"); t.unlink(missing_ok=True)
        except Exception:
            QMessageBox.critical(self, "Not writable", f"Cannot write to: {dest}")
            return

        # Persist form
        self._save_form()

        # Build options (manual run = normal behavior, not forcing sentry flags)
        opts = RunOptions(
            dest=dest,
            fmt=self.format.currentText(),
            parallel=self.parallel.value(),
            force=self.force.isChecked(),
            extra=self.extra.text().strip(),
            m3u_export=self._read_bool(KEYS["m3u_export"], True),
            m3u_in_folder_when_single=self._read_bool(KEYS["m3u_in_folder_when_single"], True),
            smart_sync=self._read_bool("smart_sync", True),
            adaptive_parallel=self._read_bool(KEYS["adaptive_parallel"], True),
            bin_override=self.s.value(KEYS["bin"], "").strip(),
            failure_delay_ms=self._read_int(KEYS.get("failure_delay_ms", "failure_delay_ms"), 2000),
            failure_delay_multiplier=self._read_float(KEYS.get("failure_delay_multiplier", "failure_delay_multiplier"), 2.0),
            failure_delay_max_ms=self._read_int(KEYS.get("failure_delay_max_ms", "failure_delay_max_ms"), 60000),
        )

        # Kick off
        try:
            self.runner.start(urls, opts)
        except RuntimeError as e:
            QMessageBox.critical(self, "spotify-dl not found", str(e))
            return

        # Lock UI controls
        self._set_running(True)
        self._save_queue_state()

    def _stop(self):
        self.runner.stop()
        self._set_running(False)
        self.backoff_label.clear()
        # reset running statuses
        for i in range(self.queue.count()):
            row = self._row(i)
            if row.status() in (QStatus.RUNNING, QStatus.PAUSED):
                row.set_status(QStatus.PENDING)
        self.tail.setVisible(False)
        self._save_queue_state()
        self._taskbar_reset()
        self.backoff_label.clear()
        if self.tray:
            self.tray.setToolTip("Idle")

    def _pause_toggle(self):
        self.runner.pause_toggle()
        if self.btn_pause.text().startswith("Pause"):
            self.btn_pause.setText("Resume queue")
        else:
            self.btn_pause.setText("Pause after current")

    def _set_running(self, running: bool):
        for w in [self.staging, self.dest, self.format, self.parallel, self.force, self.extra,
                  self.btn_history, self.btn_settings, self.btn_add, self.btn_remove, self.btn_clear,
                  self.btn_up, self.btn_down, self.btn_import, self.btn_export,
                  getattr(self, 'btn_retry_failed', None), getattr(self, 'btn_remove_completed', None)]:
            if w is not None:
                w.setEnabled(not running)
        self.queue.setEnabled(not running)
        self.btn_run.setEnabled(not running)
        self.btn_stop.setEnabled(running)
        self.btn_pause.setEnabled(running)
        if running:
            self.btn_pause.setText("Pause after current")
        self.btn_run.setText("Run" if not running else "Running…")

    # --------------- Runner signal handlers ---------------
    def _on_job_started(self, idx: int, total: int, url: str):
        if 0 <= idx < self.queue.count():
            row = self._row(idx)
            row.set_status(QStatus.RUNNING); row.set_progress(0)
            self.queue.setCurrentRow(idx)
        self.tail.clear(); self.tail.setVisible(True)
        self._notify("Starting", f"Job {idx+1}/{total}")
        self._current_job_started_ts = time.time()
        self._current_job_total = total
        self._current_job_index = idx
        self._update_taskbar_progress(0)
        self._update_tray_tooltip(0)
        self.backoff_label.clear()

    def _on_job_log(self, idx: int, chunk: str):
        tc = self.tail.textCursor()
tc.movePosition(QTextCursor.End)
self.tail.setTextCursor(tc)
        self.tail.insertPlainText(chunk)
        lower = chunk.lower()
        if "[rate-limit" in lower:
            for line in chunk.splitlines():
                if "[rate-limit" in line.lower():
                    self.backoff_label.setText(line.strip())
                    break

    def _on_job_progress(self, idx: int, pct: int):
        if 0 <= idx < self.queue.count():
            self._row(idx).set_progress(pct)
        # elapsed / ETA
        if getattr(self, "_current_job_started_ts", None):
            try:
                elapsed = int(time.time() - float(self._current_job_started_ts))
            except Exception:
                elapsed = 0
            eta = None
            if pct > 0:
                remaining = elapsed * (100 - pct) / max(1, pct)
                try:
                    eta = int(remaining)
                except Exception:
                    eta = None
            self.time_label.setText(self._fmt_elapsed_eta(elapsed, eta))
        self._update_taskbar_progress(pct)
        self._update_tray_tooltip(pct)

    def _on_job_finished(self, idx: int, summary):
        if 0 <= idx < self.queue.count():
            self._row(idx).set_status(QStatus.OK if int(summary.code) == 0 else QStatus.FAIL)
        self._append_history(summary)
        self._save_queue_state()

        sus = len(summary.suspects or [])
        body = f"{'OK' if int(summary.code) == 0 else 'Failed'} — files: {len(summary.outputs)}"
        if sus: body += f"; suspects: {sus}"
        if summary.log_path:
            body += f"\nLog: {Path(summary.log_path).name}"
        self._notify("Download finished", body, 6000)
        # reset job timer label
        self.time_label.setText("")
        self._update_taskbar_progress(0 if int(summary.code) != 0 else 100)
        self._update_tray_tooltip(100)

    def _on_queue_finished(self, totals):
        self._set_running(False)
        self.tail.setVisible(False)
        self._notify("Queue complete",
                     f"OK: {totals.ok}  •  Fail: {totals.fail}  •  Suspects: {totals.suspect}",
                     7000)
        self._taskbar_reset()
        self.backoff_label.clear()
        if self.tray:
            self.tray.setToolTip("Idle")
        if totals.ok > 0 and self._read_bool(KEYS["open_when_done"], False):
            d = self.dest.text().strip()
            if d and Path(d).exists():
                self._open_path(d)

    def _on_backoff_updated(self, remaining: int):
        if remaining > 0:
            self.backoff_label.setText(f"Cooling down {remaining}s to avoid rate limits…")
        else:
            self.backoff_label.clear()

    def _on_rate_limit_notice(self, message: str):
        msg = (message or "").strip()
        if msg:
            self.backoff_label.setText(msg)
        else:
            self.backoff_label.clear()

    def _on_parallel_changed(self, eff: int):
        self.adapt_label.setText(f"Adaptive parallel: {eff}")

    def _on_command_line(self, idx: int, cmd: str):
        if platform.system() == "Windows" and self._read_bool(KEYS["persistent_terminal"], False):
            self._ensure_persistent_terminal(start_hidden=True)
            self._send_to_persistent(cmd)
        self._last_cmd = cmd
    
        # --------------- Pending selection ---------------
    def _pending_urls(self) -> list[str]:
        """Return only URLs that haven't been processed yet."""
        urls = []
        for i in range(self.queue.count()):
            row = self._row(i)
            # Only run items that haven't completed/failed yet
            if row.status() in (QStatus.PENDING,):
                urls.append(row.url())
        return urls

    # --------------- Queue persistence ---------------
    def _save_queue_state(self) -> None:
        try:
            items = []
            for i in range(self.queue.count()):
                row = self._row(i)
                st = row.status()
                # Normalize transient states to PENDING
                if st in (QStatus.RUNNING, QStatus.PAUSED):
                    st_name = QStatus.PENDING.name
                else:
                    st_name = st.name
                items.append({"url": row.url(), "status": st_name})
            self.s.setValue(KEYS.get("queue_state", "queue_state"), json.dumps(items))
        except Exception:
            pass

    def _restore_queue_state(self) -> None:
        try:
            raw = self.s.value(KEYS.get("queue_state", "queue_state"), "[]")
            data = json.loads(raw) if raw else []
        except Exception:
            data = []
        if not isinstance(data, list):
            return
        if self.queue.count() > 0:
            return  # don't overwrite an existing live queue
        for entry in data:
            try:
                url = (entry.get("url") or "").strip()
                st_name = (entry.get("status") or "PENDING").upper()
                st = QStatus[st_name] if st_name in QStatus.__members__ else QStatus.PENDING
                if st in (QStatus.RUNNING, QStatus.PAUSED):
                    st = QStatus.PENDING
                if not url:
                    continue
                roww = QueueRow(url, st)
                item = QListWidgetItem(self.queue)
                item.setSizeHint(roww.sizeHint())
                self.queue.addItem(item)
                self.queue.setItemWidget(item, roww)
            except Exception:
                continue

    def _clear_queue(self):
        self.queue.clear()
        self._save_queue_state()

    def _retry_failed(self):
        changed = 0
        for i in range(self.queue.count()):
            row = self._row(i)
            if row.status() == QStatus.FAIL:
                row.set_status(QStatus.PENDING)
                changed += 1
        if changed:
            self._notify("Retry queued", f"Reset {changed} failed item(s).", 2500)
            self._save_queue_state()

    def _remove_completed(self):
        removed = 0
        for i in reversed(range(self.queue.count())):
            row = self._row(i)
            if row.status() in (QStatus.OK, QStatus.SKIPPED):
                self.queue.takeItem(i)
                removed += 1
        if removed:
            self._notify("Queue cleaned", f"Removed {removed} completed item(s).", 2500)
            self._save_queue_state()

    # --------------- History persistence ---------------
    def _load_history(self):
        try:
            return json.loads(self.s.value(KEYS["history"], "[]"))
        except Exception:
            return []

    def _save_history(self, hist):
        try:
            try:
                cap = int(self.s.value(KEYS.get("history_max", "history_max"), 100))
            except Exception:
                cap = 100
            cap = max(10, int(cap))
            self.s.setValue(KEYS["history"], json.dumps(hist[-cap:]))
        except Exception:
            pass

    def _append_history(self, summary):
        hist = self._load_history()
        first = (summary.outputs[0] if summary.outputs else {}) or {}
        entry = {
            "start_iso": summary.start_iso,
            "code": int(summary.code),
            "dest": summary.dest,
            "log_path": summary.log_path or "",
            "input": summary.url,
            "urls": 1,
            "moved": summary.stats.get("moved", 0),
            "replaced": summary.stats.get("replaced", 0),
            "deleted": summary.stats.get("deleted", 0),
            "skipped": summary.stats.get("skipped", 0),
            "suspect": len(summary.suspects or []),
            "first_artist": first.get("artist", ""),
            "first_album": first.get("album", ""),
        }
        hist.append(entry)
        self._save_history(hist)

    # --------------- Settings / History dialogs ---------------
    def open_settings(self):
        dlg = SettingsDialog(self)
        if dlg.exec():
            if platform.system() == "Windows" and self._read_bool(KEYS["persistent_terminal"], False):
                self._ensure_persistent_terminal(start_hidden=True)
            self._update_bin_pill()

            # Re-read Sentry config from settings
            self._sentry_enabled = self._read_bool(KEYS.get("sentry_enabled", "sentry_enabled"), False)
            try:
                self._sentry_gap_sec = max(25, int(self.s.value(KEYS.get("sentry_gap_sec", "sentry_gap_sec"), 25)))
            except Exception:
                self._sentry_gap_sec = 25
            self._update_sentry_indicator()

    def open_history(self):
        dlg = HistoryDialog(self, self._load_history())
        dlg.sig_requeue.connect(lambda urls: self._add_urls(urls, dedupe_history=False))
        dlg.exec()


    # --------------- Clipboard watcher ---------------
    def _tick_clipboard(self):
        txt = QApplication.clipboard().text().strip()
        if not txt or txt == getattr(self, "_last_clip", ""):
            return
        self._last_clip = txt

        # Only auto-add if user opted-in OR Sentry is enabled
        if not (self.auto_clip.isChecked() or self._sentry_enabled):
            return

        candidates = [s for s in re.split(r"[\s\r\n]+", txt) if s]
        urls = [u for u in candidates if SPOTIFY_URL_RE.match(u)]
        if not urls:
            return

        # If Sentry is ON, de-dupe against successful history
        self._add_urls(urls, dedupe_history=self._sentry_enabled)

        # Auto-run when idle under Sentry
        if self._sentry_enabled and not self.runner.is_running() and self.queue.count() > 0:
            dest = self.dest.text().strip()
            if not dest:
                return
            self._save_form()
            # Build options including sentry fields (Runner enforces inter-job gap)
            try:
                opts = RunOptions(
                    dest=dest,
                    fmt=self.format.currentText(),
                    parallel=self.parallel.value(),
                    force=self.force.isChecked(),
                    extra=self.extra.text().strip(),
                    m3u_export=self._read_bool(KEYS["m3u_export"], True),
                    m3u_in_folder_when_single=self._read_bool(KEYS["m3u_in_folder_when_single"], True),
                    smart_sync=self._read_bool("smart_sync", True),
                    adaptive_parallel=self._read_bool(KEYS["adaptive_parallel"], True),
                    bin_override=self.s.value(KEYS["bin"], "").strip(),
                    # Sentry
                    sentry_enabled=True,
                    sentry_gap_sec=max(25, int(self._sentry_gap_sec)),
                    failure_delay_ms=self._read_int(KEYS.get("failure_delay_ms", "failure_delay_ms"), 2000),
                    failure_delay_multiplier=self._read_float(KEYS.get("failure_delay_multiplier", "failure_delay_multiplier"), 2.0),
                    failure_delay_max_ms=self._read_int(KEYS.get("failure_delay_max_ms", "failure_delay_max_ms"), 60000),
                )
                pending = self._pending_urls()
                if not pending:
                    return
                # Enforce conservative pacing in Sentry mode
                opts.parallel = 1
                base_delay = max(25000, int(opts.failure_delay_ms))
                first_url = pending[0]
                if self._is_album_or_playlist(first_url):
                    base_delay = max(25000, base_delay)
                opts.failure_delay_ms = base_delay
                opts.failure_delay_multiplier = max(1.0, float(opts.failure_delay_multiplier))
                opts.failure_delay_max_ms = max(int(opts.failure_delay_max_ms), opts.failure_delay_ms)
                self.runner.start(pending, opts)

                self._set_running(True)
            except RuntimeError as e:
                QMessageBox.critical(self, "spotify-dl not found", str(e))

    # --------------- Scheduler ---------------
    def _tick_scheduler(self):
        if not self._read_bool(KEYS["scheduler_enabled"], False):
            return
        hhmm = (self.s.value(KEYS["scheduler_time"], "") or "").strip()
        if not hhmm or not re.match(r"^\d{2}:\d{2}$", hhmm):
            return
        now = QTime.currentTime().toString("HH:mm")
        if now != hhmm:
            return
        if self.runner.is_running() or self.queue.count() == 0:
            return
        self._run()

    # --------------- Sentry toggle (tray) ---------------
    def _toggle_sentry(self, on: bool):
        self._sentry_enabled = bool(on)
        self.s.setValue(KEYS.get("sentry_enabled", "sentry_enabled"), "true" if self._sentry_enabled else "false")
        try:
            self._sentry_gap_sec = max(25, int(self.s.value(KEYS.get("sentry_gap_sec", "sentry_gap_sec"), 25)))
        except Exception:
            self._sentry_gap_sec = 25
        self._update_sentry_indicator()

    # --------------- Persistent terminal (Windows) ---------------
    def _ensure_persistent_terminal(self, start_hidden=False):
        if platform.system() != "Windows":
            return
        if self._persist_proc and self._persist_proc.poll() is None:
            if self._persist_hwnd and start_hidden:
                show_window(self._persist_hwnd, False)
            return
        creation = subprocess.CREATE_NEW_CONSOLE | subprocess.CREATE_NEW_PROCESS_GROUP
        try:
            self._persist_proc = subprocess.Popen(
                ["cmd.exe", "/k", "title spotify-dl GUI Terminal"],
                creationflags=creation,
                stdin=subprocess.PIPE,
                cwd=os.getcwd(),
                close_fds=False
            )
        except Exception as e:
            QMessageBox.critical(self, "Persistent terminal", f"Failed to open terminal:\n{e}")
            self._persist_proc = None; self._persist_hwnd = None; return
        self._persist_hwnd = None
        def _grab_hwnd():
            if not self._persist_proc:
                return
            hwnd = console_hwnd_for_pid(self._persist_proc.pid)
            if hwnd:
                self._persist_hwnd = hwnd
                show_window(hwnd, not start_hidden)
        QTimer.singleShot(300, _grab_hwnd)

        try:
            exe = resolve_spotifydl_binary(self.s)
            self._send_to_persistent(exe)
        except Exception:
            pass

    def _send_to_persistent(self, command: str):
        if platform.system() != "Windows":
            return
        if not self._persist_proc or self._persist_proc.poll() is not None:
            self._ensure_persistent_terminal(start_hidden=False)
        try:
            if self._persist_proc and self._persist_proc.stdin:
                self._persist_proc.stdin.write((command + "\r\n").encode("utf-8", errors="ignore"))
                self._persist_proc.stdin.flush()
        except Exception:
            pass

    # --------------- Tray ---------------
    def _tray_show(self):
        self.show(); self.raise_(); self.activateWindow()

    def closeEvent(self, event):
        if self._really_quit:
            event.accept()
            return
        # existing minimize-to-tray behavior
        if self._read_bool(KEYS["minimize_to_tray"], True) and QSystemTrayIcon.isSystemTrayAvailable():
            event.ignore()
            self.hide()
            self._notify("Still running in tray", "Downloads continue in the background.", 3500)
        else:
            event.accept()

    @staticmethod
    def _is_album_or_playlist(url: str) -> bool:
        u = (url or '').lower().strip()
        return ('/album/' in u or u.startswith('spotify:album:') or
                '/playlist/' in u or u.startswith('spotify:playlist:'))

    # --------------- Utilities ---------------
    @staticmethod
    def _open_path(path: str) -> None:
        try:
            if sys.platform.startswith("win"):
                os.startfile(path)  # type: ignore[attr-defined]
            elif sys.platform == "darwin":
                subprocess.Popen(["open", path])
            else:
                subprocess.Popen(["xdg-open", path])
        except Exception:
            pass

    # Drag-drop plain text support onto the queue list
    def dragEnterEvent(self, e):
        if e.mimeData().hasText():
            e.acceptProposedAction()
        else:
            super().dragEnterEvent(e)

    def dropEvent(self, e):
        if e.mimeData().hasText():
            txt = e.mimeData().text()
            urls = [s.strip() for s in re.split(r"[\s\r\n]+", txt) if s.strip()]
            urls = [u for u in urls if SPOTIFY_URL_RE.match(u)]
            if urls:
                self._add_urls(urls, dedupe_history=False)
                self._save_queue_state()
            e.acceptProposedAction()
        else:
            super().dropEvent(e)

    # --------------- Taskbar/tray helpers ---------------
    def _ensure_taskbar(self):
        if platform.system() != "Windows" or self._taskbar_btn is not None:
            return
        try:
            from PySide6.QtWinExtras import QWinTaskbarButton
            self._taskbar_btn = QWinTaskbarButton(self)
            self._taskbar_btn.setWindow(self.windowHandle())
            self._taskbar_prog = self._taskbar_btn.progress()
            self._taskbar_prog.setRange(0, 100)
            self._taskbar_prog.reset()
        except Exception:
            self._taskbar_btn = None
            self._taskbar_prog = None

    def _update_taskbar_progress(self, pct: int):
        self._ensure_taskbar()
        if self._taskbar_prog:
            try:
                self._taskbar_prog.show()
                self._taskbar_prog.setValue(int(max(0, min(100, pct))))
            except Exception:
                pass

    def _taskbar_reset(self):
        if self._taskbar_prog:
            try:
                self._taskbar_prog.reset()
            except Exception:
                pass

    def _update_tray_tooltip(self, pct: int):
        if not self.tray:
            return
        try:
            if self.runner and self.runner.is_running():
                idx = self._current_job_index + 1 if self._current_job_index >= 0 else 1
                total = max(1, int(self._current_job_total or 1))
                tip = f"Job {idx}/{total} — {pct}%"
                if self.time_label.text():
                    tip += f" — {self.time_label.text()}"
                self.tray.setToolTip(tip)
            else:
                self.tray.setToolTip("Idle")
        except Exception:
            pass

    @staticmethod
    def _fmt_secs(n: int) -> str:
        h = n // 3600
        m = (n % 3600) // 60
        s = n % 60
        if h > 0:
            return f"{h:d}:{m:02d}:{s:02d}"
        return f"{m:d}:{s:02d}"

    def _fmt_elapsed_eta(self, elapsed: int, eta: int | None) -> str:
        if eta is None:
            return f"Elapsed {self._fmt_secs(elapsed)}"
        return f"Elapsed {self._fmt_secs(elapsed)} • ETA {self._fmt_secs(max(0, int(eta)))}"
