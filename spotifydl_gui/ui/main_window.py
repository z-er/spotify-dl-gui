# spotifydl_gui/ui/main_window.py
"""MainWindow rewritten for job-based queue management."""

from __future__ import annotations

import json
import os
import platform
import re
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Tuple

from PySide6.QtCore import Qt, QTimer, QTime, Signal, Slot
from PySide6.QtGui import QAction, QIcon, QTextCursor
from PySide6.QtWidgets import (
    QApplication,
    QCheckBox,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMenu,
    QMessageBox,
    QPushButton,
    QComboBox,
    QSpinBox,
    QSystemTrayIcon,
    QTextEdit,
    QVBoxLayout,
    QWidget,
)

from ..job_queue import JobQueue
from ..job_types import Job, JobItem, JobItemState, JobState, QueueSource
from ..runner import Runner, RunOptions, SPOTIFY_URL_RE
from ..settings_store import APP_NAME, APP_VER, KEYS, get_settings
from ..utils import console_hwnd_for_pid, get_app_icon, resolve_spotifydl_binary, show_window
from ..web_server import WebQueueServer
from .history_dialog import HistoryDialog
from .job_item_row import JobItemRow
from .job_row import JobRow
from .settings_dialog import SettingsDialog
from .shortcuts_dialog import ShortcutsDialog


CLIPBOARD_POLL_MS = 1000
AUTO_REMOVE_SOURCES = {QueueSource.WEB, QueueSource.SENTRY}


class MainWindow(QWidget):
    sig_web_enqueue = Signal(list, str)

    def __init__(self, app_icon: QIcon | None = None):
        super().__init__()
        self.s = get_settings()
        self.setWindowTitle(f"{APP_NAME} • {APP_VER}")
        self.setMinimumSize(1180, 780)
        if app_icon:
            self.setWindowIcon(app_icon)

        self._really_quit = False
        self._job_widgets: Dict[int, Tuple[QListWidgetItem, JobRow]] = {}
        self._job_item_widgets: Dict[int, Dict[int, Tuple[QListWidgetItem, JobItemRow]]] = {}
        self._last_clip = ""
        self._current_job_id: Optional[int] = None
        self._persist_proc: Optional[subprocess.Popen] = None
        self._persist_hwnd = None
        self._web_server: Optional[WebQueueServer] = None
        self._taskbar_btn = None
        self._taskbar_prog = None
        self._last_cmd = ""
        self._current_job_started_ts: Optional[float] = None
        self._queue_paused = False
        self._last_run_summary = "Never"

        self.job_queue = JobQueue(self.s, self)
        self.runner = Runner(self.s, self.job_queue, self)

        self._build_ui(app_icon or get_app_icon())
        self._connect_signals()

        self._load_form()
        self._update_bin_pill()
        self._update_sentry_indicator()
        self._restore_jobs()
        self._configure_web_server()
        self._install_shortcuts()

        self._clip_timer = QTimer(self)
        self._clip_timer.setInterval(CLIPBOARD_POLL_MS)
        self._clip_timer.timeout.connect(self._tick_clipboard)
        self._clip_timer.start()

        self._sched_timer = QTimer(self)
        self._sched_timer.setInterval(30_000)
        self._sched_timer.timeout.connect(self._tick_scheduler)
        self._sched_timer.start()

        if platform.system() == "Windows" and self._read_bool(KEYS["persistent_terminal"], False):
            self._ensure_persistent_terminal(start_hidden=True)

    # ------------------------------------------------------------------
    # UI construction
    # ------------------------------------------------------------------
    def _build_ui(self, app_icon: Optional[QIcon]) -> None:
        self.header_icon = QLabel()
        self.header_icon.setFixedSize(32, 32)
        self.header_icon.setScaledContents(True)
        if app_icon and not app_icon.isNull():
            self.header_icon.setPixmap(app_icon.pixmap(32, 32))
            self.header_icon.setToolTip(f"{APP_NAME} {APP_VER}")
        else:
            self.header_icon.setVisible(False)

        title = QLabel(f"spotify-dl — simple GUI  •  {APP_VER}")
        title.setStyleSheet("font-size: 18px; font-weight: 600;")
        self.btn_history = QPushButton("History")
        self.btn_history.clicked.connect(self.open_history)
        self.btn_shortcuts = QPushButton("Shortcuts")
        self.btn_shortcuts.clicked.connect(self.open_shortcuts)
        self.btn_settings = QPushButton("Settings")
        self.btn_settings.clicked.connect(self.open_settings)
        self.btn_quit = QPushButton("Quit")
        self.btn_quit.clicked.connect(self._quit)

        hdr = QHBoxLayout()
        if self.header_icon.isVisible():
            hdr.addWidget(self.header_icon)
            hdr.addSpacing(6)
        hdr.addWidget(title)
        hdr.addStretch()
        hdr.addWidget(self.btn_history)
        hdr.addWidget(self.btn_shortcuts)
        hdr.addWidget(self.btn_settings)
        hdr.addWidget(self.btn_quit)

        # --- Job list + controls ---
        self.job_list = QListWidget()
        self.job_list.setSelectionMode(QListWidget.SingleSelection)
        self.job_list.setSpacing(6)
        self.job_list.setContextMenuPolicy(Qt.CustomContextMenu)
        self.job_list.customContextMenuRequested.connect(self._job_context_menu)

        self.job_item_list = QListWidget()
        self.job_item_list.setSelectionMode(QListWidget.SingleSelection)
        self.job_item_list.setSpacing(4)

        self.btn_add_job = QPushButton("Add URLs ➜ Job")
        self.btn_add_job.clicked.connect(self._add_from_staging)
        self.btn_remove_job = QPushButton("Remove job")
        self.btn_remove_job.clicked.connect(self._remove_selected_jobs)
        self.btn_clear_jobs = QPushButton("Clear jobs")
        self.btn_clear_jobs.clicked.connect(self._clear_jobs)
        self.btn_up = QPushButton("↑")
        self.btn_up.clicked.connect(lambda: self._nudge_job(-1))
        self.btn_down = QPushButton("↓")
        self.btn_down.clicked.connect(lambda: self._nudge_job(+1))

        job_controls = QHBoxLayout()
        job_controls.addWidget(self.btn_add_job)
        job_controls.addStretch()
        job_controls.addWidget(self.btn_up)
        job_controls.addWidget(self.btn_down)
        job_controls.addSpacing(8)
        job_controls.addWidget(self.btn_remove_job)
        job_controls.addWidget(self.btn_clear_jobs)

        self.btn_run = QPushButton("Run")
        self.btn_run.clicked.connect(self._run)
        self.btn_pause = QPushButton("Pause after current")
        self.btn_pause.setEnabled(False)
        self.btn_pause.clicked.connect(self._pause_toggle)
        self.btn_stop = QPushButton("Stop")
        self.btn_stop.setEnabled(False)
        self.btn_stop.clicked.connect(self._stop)
        self.btn_retry_failed = QPushButton("Retry failed")
        self.btn_retry_failed.clicked.connect(self._retry_failed_items)
        self.btn_remove_completed = QPushButton("Remove completed")
        self.btn_remove_completed.clicked.connect(self._remove_completed_jobs)

        run_row = QHBoxLayout()
        run_row.addWidget(self.btn_run)
        run_row.addWidget(self.btn_pause)
        run_row.addWidget(self.btn_stop)
        run_row.addSpacing(8)
        run_row.addWidget(self.btn_retry_failed)
        run_row.addWidget(self.btn_remove_completed)
        run_row.addStretch()

        left = QVBoxLayout()
        left.addLayout(hdr)
        left.addSpacing(6)
        left.addWidget(QLabel("Jobs"))
        left.addWidget(self.job_list, 1)
        left.addLayout(job_controls)
        left.addSpacing(6)
        left.addWidget(QLabel("Job items"))
        left.addWidget(self.job_item_list, 1)
        left.addSpacing(6)
        left.addLayout(run_row)

        # --- Right side (staging, options, tail) ---
        self.staging = QTextEdit()
        self.staging.setAcceptRichText(False)
        self.staging.setPlaceholderText("Paste Spotify URLs here (one per line), then click ‘Add URLs ➜ Job’")

        self.dest = QLineEdit()
        self.dest.setPlaceholderText("Choose destination folder")
        self.btn_dest = QPushButton("Browse…")
        self.btn_dest.clicked.connect(self._pick_dest)

        dest_row = QHBoxLayout()
        dest_row.addWidget(QLabel("Destination"))
        dest_row.addSpacing(6)
        dest_row.addWidget(self.dest, 1)
        dest_row.addWidget(self.btn_dest)

        self.format = QComboBox()
        self.format.addItems(["flac", "mp3", "m4a", "opus"])
        self.parallel = QSpinBox()
        self.parallel.setRange(1, 32)
        self.parallel.setValue(5)
        self.force = QCheckBox("Force re-download")

        opt_row = QHBoxLayout()
        opt_row.addWidget(QLabel("Format"))
        opt_row.addWidget(self.format)
        opt_row.addSpacing(16)
        opt_row.addWidget(QLabel("Parallel"))
        opt_row.addWidget(self.parallel)
        opt_row.addSpacing(16)
        opt_row.addWidget(self.force)
        opt_row.addStretch()

        self.extra = QLineEdit()
        self.extra.setPlaceholderText("Optional extra flags (advanced)")

        self.backoff_label = QLabel("")
        self.backoff_label.setProperty("class", "muted")
        self.adapt_label = QLabel("")
        self.adapt_label.setProperty("class", "muted")
        self.time_label = QLabel("")
        self.time_label.setProperty("class", "muted")

        util_row = QHBoxLayout()
        util_row.addWidget(self.adapt_label)
        util_row.addSpacing(8)
        util_row.addWidget(self.time_label)
        util_row.addStretch()
        util_row.addWidget(self.backoff_label)

        self.tail = QTextEdit()
        self.tail.setReadOnly(True)
        self.tail.setVisible(False)

        disclaimer = QLabel("Requires Spotify Premium. Use at your own risk — may violate Spotify Terms or local laws.")
        disclaimer.setWordWrap(True)
        disclaimer.setProperty("class", "muted")

        self.bin_pill = QLabel()
        self.bin_pill.setObjectName("binPill")
        self._sentry_label = QLabel("")
        self._sentry_label.setProperty("class", "muted")
        right_footer = QHBoxLayout()
        right_footer.addWidget(self._sentry_label)
        right_footer.addStretch()
        right_footer.addWidget(self.bin_pill)

        self.auto_clip = QCheckBox("Auto-add clipboard links")
        self.auto_clip.setChecked(True)

        right = QVBoxLayout()
        right.addWidget(QLabel("Tracks / Playlists / Albums"))
        right.addWidget(self.staging)
        right.addSpacing(6)
        right.addLayout(dest_row)
        right.addSpacing(4)
        right.addLayout(opt_row)
        right.addSpacing(6)
        right.addWidget(QLabel("Extra flags"))
        right.addWidget(self.extra)
        right.addSpacing(10)
        right.addWidget(self.auto_clip)
        right.addSpacing(6)
        right.addLayout(util_row)
        right.addSpacing(6)
        right.addWidget(self.tail, 1)
        right.addSpacing(6)
        right.addWidget(disclaimer)
        right.addSpacing(6)
        right.addLayout(right_footer)

        layout = QHBoxLayout(self)
        layout.addLayout(left, 0)
        layout.addSpacing(12)
        layout.addLayout(right, 1)

        self._setup_tray(app_icon)

    def _setup_tray(self, app_icon: Optional[QIcon]) -> None:
        self.tray: Optional[QSystemTrayIcon] = None
        if not QSystemTrayIcon.isSystemTrayAvailable():
            return

        icon = app_icon or get_app_icon()
        self.tray = QSystemTrayIcon(self)
        self.tray.setIcon(icon)
        menu = QMenu()
        act_show = QAction("Show", self)
        act_show.triggered.connect(self._tray_show)
        act_quit = QAction("Quit", self)
        act_quit.setShortcut("Ctrl+Q")
        act_quit.triggered.connect(self._quit)

        self._sentry_enabled = self._read_bool(KEYS.get("sentry_enabled", "sentry_enabled"), False)
        try:
            self._sentry_gap_sec = max(25, int(self.s.value(KEYS.get("sentry_gap_sec", "sentry_gap_sec"), 25)))
        except Exception:
            self._sentry_gap_sec = 25

        act_sentry = QAction("Sentry mode", self)
        act_sentry.setCheckable(True)
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

    # ------------------------------------------------------------------
    # Connections
    # ------------------------------------------------------------------
    def _connect_signals(self) -> None:
        self.job_list.itemSelectionChanged.connect(self._on_job_selection_changed)

        self.job_queue.job_added.connect(self._on_job_added)
        self.job_queue.job_removed.connect(self._on_job_removed)
        self.job_queue.job_updated.connect(self._on_job_updated)
        self.job_queue.active_job_changed.connect(self._on_active_job_changed)

        self.runner.sig_job_started.connect(self._on_job_started)
        self.runner.sig_job_item_started.connect(self._on_job_item_started)
        self.runner.sig_job_item_log.connect(self._on_job_item_log)
        self.runner.sig_job_item_progress.connect(self._on_job_item_progress)
        self.runner.sig_job_item_finished.connect(self._on_job_item_finished)
        self.runner.sig_job_finished.connect(self._on_job_finished)
        self.runner.sig_queue_finished.connect(self._on_queue_finished)
        self.runner.sig_backoff_updated.connect(self._on_backoff_updated)
        self.runner.sig_command_line.connect(self._on_command_line)
        self.runner.sig_parallel_changed.connect(self._on_parallel_changed)
        self.runner.sig_rate_limit_notice.connect(self._on_rate_limit_notice)

        self.sig_web_enqueue.connect(self.handle_web_submission)

    # ------------------------------------------------------------------
    # Settings helpers
    # ------------------------------------------------------------------
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

    def _load_form(self) -> None:
        self.dest.setText(self.s.value(KEYS["dest"], ""))
        self.format.setCurrentText(self.s.value(KEYS["format"], "flac"))
        try:
            self.parallel.setValue(int(self.s.value(KEYS["parallel"], 5)))
        except Exception:
            self.parallel.setValue(5)
        self.force.setChecked(self._read_bool(KEYS["force"], False))
        self.extra.setText(self.s.value(KEYS["extra"], ""))

    def _save_form(self) -> None:
        self.s.setValue(KEYS["dest"], self.dest.text().strip())
        self.s.setValue(KEYS["format"], self.format.currentText())
        self.s.setValue(KEYS["parallel"], self.parallel.value())
        self.s.setValue(KEYS["force"], "true" if self.force.isChecked() else "false")
        self.s.setValue(KEYS["extra"], self.extra.text().strip())

    # ------------------------------------------------------------------
    # Job queue persistence/UI sync
    # ------------------------------------------------------------------
    def _restore_jobs(self) -> None:
        for job in self.job_queue.jobs():
            self._insert_job_widget(job)
        if self.job_queue.active_job():
            self._highlight_job(self.job_queue.active_job().job_id)

    def _insert_job_widget(self, job: Job) -> None:
        item = QListWidgetItem(self.job_list)
        row = JobRow(job)
        item.setSizeHint(row.sizeHint())
        self.job_list.addItem(item)
        self.job_list.setItemWidget(item, row)
        self._job_widgets[job.job_id] = (item, row)

    def _on_job_added(self, job: Job) -> None:
        if job.job_id in self._job_widgets:
            self._on_job_updated(job)
            return
        self._insert_job_widget(job)
        self._notify("Job queued", f"{len(job.items)} URL(s) added.", 2500)
        self._maybe_start_next_job()

    def _on_job_removed(self, job_id: int) -> None:
        tup = self._job_widgets.pop(job_id, None)
        if not tup:
            return
        item, _ = tup
        row = self.job_list.row(item)
        self.job_list.takeItem(row)
        self._job_item_widgets.pop(job_id, None)
        if not self.job_list.count():
            self.job_item_list.clear()

    def _on_job_updated(self, job: Job) -> None:
        tup = self._job_widgets.get(job.job_id)
        if not tup:
            self._insert_job_widget(job)
            tup = self._job_widgets[job.job_id]
        _, row = tup
        row.update_from_job(job)
        if self._current_job_id == job.job_id:
            self._render_job_items(job)

    def _on_active_job_changed(self, job: Optional[Job]) -> None:
        if job:
            self._highlight_job(job.job_id)
        else:
            self.job_list.clearSelection()

    def _highlight_job(self, job_id: int) -> None:
        tup = self._job_widgets.get(job_id)
        if not tup:
            return
        item, _ = tup
        self.job_list.setCurrentItem(item)
        self._current_job_id = job_id

    def _maybe_start_next_job(self, delay_ms: int = 0) -> None:
        if self._queue_paused:
            return

        def evaluate():
            if self._queue_paused:
                return
            if self.runner.is_running():
                QTimer.singleShot(100, evaluate)
                return
            next_job = self.job_queue.next_pending_job()
            if next_job:
                self._start_job(next_job)

        if delay_ms > 0:
            QTimer.singleShot(delay_ms, evaluate)
        else:
            QTimer.singleShot(0, evaluate)

    # ------------------------------------------------------------------
    # UI actions
    # ------------------------------------------------------------------
    def _add_from_staging(self) -> None:
        text = self.staging.toPlainText().strip()
        if not text:
            QMessageBox.information(self, "Nothing to add", "Paste Spotify URLs first.")
            return
        urls = [ln.strip() for ln in text.splitlines() if ln.strip()]
        ok, msg = self._create_job_from_urls(urls, QueueSource.MANUAL)
        if ok:
            self.staging.clear()
            self._save_form()
            self._notify("Queued job", msg, 2500)
        else:
            QMessageBox.warning(self, "Add failed", msg)

    def _create_job_from_urls(
        self,
        urls: Iterable[str],
        source: QueueSource,
        dest_override: str = "",
        auto_run: bool = False,
    ) -> Tuple[bool, str]:
        validated = []
        seen = set()
        for url in urls:
            url = (url or "").strip()
            if not url or url in seen:
                continue
            if not SPOTIFY_URL_RE.match(url):
                continue
            seen.add(url)
            validated.append(url)
        if not validated:
            return False, "No valid Spotify URLs supplied."

        opts = self._build_run_options(dest_override)
        if not opts.dest:
            return False, "Destination folder is required."

        label = self._suggest_job_label(validated)
        job = self.job_queue.add_job(validated, label=label, source=source, options=opts.to_payload(), auto_remove=source in AUTO_REMOVE_SOURCES)
        if auto_run and not self.runner.is_running():
            self._start_job(job)
        return True, f"Job '{label}' with {len(validated)} URLs queued."

    def _suggest_job_label(self, urls: List[str]) -> str:
        first = urls[0] if urls else "Job"
        if "open.spotify.com" in first:
            slug = first.rstrip('/').split('/')[-1]
        elif ':' in first:
            slug = first.split(':')[-1]
        else:
            slug = first
        return f"{slug} ({len(urls)} item{'s' if len(urls) != 1 else ''})"

    def _build_run_options(self, dest_override: str = "") -> RunOptions:
        dest = dest_override or self.dest.text().strip()
        return RunOptions(
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
            sentry_enabled=getattr(self, "_sentry_enabled", False),
            sentry_gap_sec=getattr(self, "_sentry_gap_sec", 25),
            json_events=True,
        )

    def _pick_dest(self):
        p = QFileDialog.getExistingDirectory(self, "Choose destination folder")
        if p:
            self.dest.setText(p)

    def _job_context_menu(self, pos) -> None:
        item = self.job_list.itemAt(pos)
        if not item:
            return
        job_id = self._job_id_from_item(item)
        menu = QMenu(self)
        act_run = QAction("Run now", self)
        act_run.triggered.connect(lambda: self._start_specific_job(job_id))
        act_remove = QAction("Remove", self)
        act_remove.triggered.connect(lambda: self._remove_job(job_id))
        act_export = QAction("Export URLs", self)
        act_export.triggered.connect(lambda: self._export_single_job(job_id))
        menu.addAction(act_run)
        menu.addAction(act_export)
        menu.addSeparator()
        menu.addAction(act_remove)
        menu.exec(self.job_list.mapToGlobal(pos))
    def _standard_confirm(self, title: str, body: str) -> bool:
        return QMessageBox.question(
            self,
            title,
            body,
            QMessageBox.Yes | QMessageBox.No,
            QMessageBox.No,
        ) == QMessageBox.Yes

    def _remove_job(self, job_id: int) -> None:
        job = self.job_queue.get_job(job_id)
        if not job:
            return
        if job.state == JobState.RUNNING:
            QMessageBox.warning(self, "Busy", "Cannot remove a running job. Stop it first.")
            return
        self.job_queue.remove_job(job_id)

    def _remove_selected_jobs(self) -> None:
        items = self.job_list.selectedItems()
        if not items:
            return
        if not self._standard_confirm("Remove jobs", "Remove the selected job(s)?"):
            return
        for item in items:
            job_id = self._job_id_from_item(item)
            self._remove_job(job_id)

    def _clear_jobs(self) -> None:
        if not self.job_list.count():
            return
        if not self._standard_confirm("Clear queue", "Remove all pending jobs?"):
            return
        if self.runner.is_running():
            QMessageBox.warning(self, "Busy", "Stop the current job before clearing.")
            return
        self.job_queue.clear()
        self.job_list.clear()
        self.job_item_list.clear()
        self._job_widgets.clear()
        self._job_item_widgets.clear()

    def _nudge_job(self, delta: int) -> None:
        current = self.job_list.currentItem()
        if not current:
            return
        row = self.job_list.row(current)
        new_row = row + delta
        if new_row < 0 or new_row >= self.job_list.count():
            return
        item = self.job_list.takeItem(row)
        self.job_list.insertItem(new_row, item)
        self.job_list.setCurrentItem(item)
        job_id = self._job_id_from_item(item)
        self.job_queue.move_job(job_id, new_row)

    def _job_id_from_item(self, item: QListWidgetItem) -> int:
        widget = self.job_list.itemWidget(item)
        if isinstance(widget, JobRow):
            return widget.job_id
        for job_id, (it, _) in self._job_widgets.items():
            if it is item:
                return job_id
        raise ValueError("Unknown job list item")

    def _render_job_items(self, job: Optional[Job]) -> None:
        self.job_item_list.clear()
        if not job:
            return
        store: Dict[int, Tuple[QListWidgetItem, JobItemRow]] = {}
        for item in job.items:
            list_item = QListWidgetItem(self.job_item_list)
            row = JobItemRow(item)
            list_item.setSizeHint(row.sizeHint())
            self.job_item_list.addItem(list_item)
            self.job_item_list.setItemWidget(list_item, row)
            store[item.item_id] = (list_item, row)
        self._job_item_widgets[job.job_id] = store

    def _on_job_selection_changed(self) -> None:
        item = self.job_list.currentItem()
        job = None
        if item:
            job_id = self._job_id_from_item(item)
            job = self.job_queue.get_job(job_id)
            self._current_job_id = job_id
        else:
            self._current_job_id = None
        self._render_job_items(job)

    # ------------------------------------------------------------------
    # Runner control
    # ------------------------------------------------------------------
    def _run(self) -> None:
        if self.runner.is_running():
            QMessageBox.information(self, "Busy", "A job is already running.")
            return
        job = None
        if self.job_list.currentItem():
            job_id = self._job_id_from_item(self.job_list.currentItem())
            job = self.job_queue.get_job(job_id)
            if job and not job.first_pending():
                job = None
        if not job:
            job = self.job_queue.next_pending_job()
        if not job:
            QMessageBox.information(self, "Nothing to do", "No pending jobs in the queue.")
            return
        self._start_job(job)

    def _start_specific_job(self, job_id: int) -> None:
        job = self.job_queue.get_job(job_id)
        if not job:
            return
        if self.runner.is_running():
            QMessageBox.warning(self, "Busy", "Another job is already running.")
            return
        if not job.first_pending():
            QMessageBox.information(self, "Already done", "This job has no pending items.")
            return
        self._start_job(job)

    def _start_job(self, job: Job) -> None:
        opts = RunOptions.from_payload(job.options)
        if not opts.dest:
            QMessageBox.critical(self, "Destination missing", "Job destination is empty.")
            return
        try:
            Path(opts.dest).mkdir(parents=True, exist_ok=True)
            probe = Path(opts.dest) / ".write_test.tmp"
            probe.write_text("ok", encoding="utf-8")
            probe.unlink(missing_ok=True)
        except Exception:
            QMessageBox.critical(self, "Not writable", f"Cannot write to: {opts.dest}")
            return
        try:
            resolve_spotifydl_binary(self.s)
        except Exception as exc:
            QMessageBox.critical(self, "spotify-dl not found", str(exc))
            return

        self._save_form()
        try:
            started = self.runner.start_job(job)
        except RuntimeError as exc:
            QMessageBox.critical(self, "Cannot start job", str(exc))
            return
        if not started:
            QMessageBox.warning(self, "Unable", "Job did not start. Perhaps it has no pending items.")
            return
        self.job_queue.set_active_job(job.job_id)
        self._set_running(True)
        self._highlight_job(job.job_id)

    def _stop(self) -> None:
        self.runner.stop()
        self._set_running(False)
        self.backoff_label.clear()
        self.tail.setVisible(False)
        self.tail.clear()

    def _pause_toggle(self) -> None:
        self.runner.pause_toggle()
        if self.btn_pause.text().startswith("Pause"):
            self.btn_pause.setText("Resume queue")
            self._queue_paused = True
        else:
            self.btn_pause.setText("Pause after current")
            self._queue_paused = False
            if not self.runner.is_running():
                next_job = self.job_queue.next_pending_job()
                if next_job:
                    self._start_job(next_job)

    def _set_running(self, running: bool) -> None:
        widgets = [
            self.staging,
            self.dest,
            self.format,
            self.parallel,
            self.force,
            self.extra,
            self.btn_history,
            self.btn_settings,
            self.btn_add_job,
            self.btn_remove_job,
            self.btn_clear_jobs,
            self.btn_up,
            self.btn_down,
        ]
        for w in widgets:
            if w is not None:
                w.setEnabled(not running)
        self.job_list.setEnabled(not running)
        self.job_item_list.setEnabled(not running)
        self.btn_run.setEnabled(not running)
        self.btn_stop.setEnabled(running)
        self.btn_pause.setEnabled(running)
        if running:
            self.btn_pause.setText("Pause after current")
            self.btn_run.setText("Running…")
            self._queue_paused = False
        else:
            self.btn_run.setText("Run")

    # ------------------------------------------------------------------
    # Runner signal handlers
    # ------------------------------------------------------------------
    def _on_job_started(self, job: Job) -> None:
        self._current_job_id = job.job_id
        self._render_job_items(job)
        self.backoff_label.clear()
        self.tail.clear()
        self.tail.setVisible(True)
        self._current_job_started_ts = time.time()
        self._update_taskbar_progress(0)
        self._update_tray_tooltip(0)
        self._notify("Starting", f"Job '{job.label}'")

    def _on_job_item_started(self, job_id: int, item_id: int, index: int, total: int, url: str) -> None:
        store = self._job_item_widgets.get(job_id)
        if store and item_id in store:
            _, row = store[item_id]
            job_item = self._find_job_item(job_id, item_id)
            if job_item:
                row.update_from_item(job_item)
        self.job_list.setToolTip(f"Running item {index}/{total}: {url}")
        self._update_taskbar_progress(0)
        self._update_tray_tooltip(0)

    def _on_job_item_log(self, job_id: int, item_id: int, chunk: str) -> None:
        tc = self.tail.textCursor()
        tc.movePosition(QTextCursor.End)
        self.tail.setTextCursor(tc)
        self.tail.insertPlainText(chunk)

    def _on_job_item_progress(self, job_id: int, item_id: int, pct: int) -> None:
        job_item = self._find_job_item(job_id, item_id)
        if job_item:
            job_item.progress = pct
        store = self._job_item_widgets.get(job_id)
        if store and item_id in store:
            _, row = store[item_id]
            if job_item:
                row.update_from_item(job_item)
        if self._current_job_started_ts:
            elapsed = int(time.time() - self._current_job_started_ts)
            eta = None
            if pct > 0:
                remaining = elapsed * (100 - pct) / max(1, pct)
                eta = int(remaining)
            self.time_label.setText(self._fmt_elapsed_eta(elapsed, eta))
        self._update_taskbar_progress(pct)
        self._update_tray_tooltip(pct)

    def _fmt_elapsed_eta(self, elapsed: int, eta: Optional[int]) -> str:
        def fmt(seconds: int) -> str:
            mins, secs = divmod(seconds, 60)
            hours, mins = divmod(mins, 60)
            if hours:
                return f"{hours}h {mins}m {secs}s"
            if mins:
                return f"{mins}m {secs}s"
            return f"{secs}s"

        txt = f"Elapsed: {fmt(elapsed)}"
        if eta is not None:
            txt += f"  •  ETA: {fmt(int(eta))}"
        return txt
    def _find_job_item(self, job_id: int, item_id: int) -> Optional[JobItem]:
        job = self.job_queue.get_job(job_id)
        if not job:
            return None
        for it in job.items:
            if it.item_id == item_id:
                return it
        return None

    def _on_job_item_finished(self, summary) -> None:
        job_id = summary.job_id
        item_id = summary.item_id
        job_item = self._find_job_item(job_id, item_id)
        if job_item:
            job_item.state = JobItemState.SUCCESS if summary.code == 0 else JobItemState.FAILED
            job_item.progress = 100 if summary.code == 0 else 0
            store = self._job_item_widgets.get(job_id)
            if store and item_id in store:
                _, row = store[item_id]
                row.update_from_item(job_item)
        if summary.log_path:
            self._notify("Download finished", f"Log saved to {Path(summary.log_path).name}", 4000)

    def _on_job_finished(self, result) -> None:
        job = result.job
        self._set_running(False)
        self.tail.setVisible(False)
        self.backoff_label.clear()
        self.time_label.clear()
        ok = result.state == JobState.SUCCESS
        msg = f"Job '{job.label}' {'completed' if ok else 'finished with issues'}"
        self._notify("Queue update", msg, 6000)
        self._update_taskbar_progress(100 if ok else 0)
        self._update_tray_tooltip(100 if ok else 0)
        self._append_history(result)
        if job.auto_remove and job.state == JobState.SUCCESS:
            self.job_queue.remove_job(job.job_id)
        else:
            self._on_job_updated(job)
        if self._read_bool(KEYS["open_when_done"], False) and ok:
            dest = RunOptions.from_payload(job.options).dest
            if dest and Path(dest).exists():
                self._open_path(dest)

        self._last_run_summary = datetime.now().strftime("%Y-%m-%d %H:%M")

        if not self._queue_paused:
            self._maybe_start_next_job()

    def _on_queue_finished(self, result) -> None:
        # compatibility stub; handled in _on_job_finished
        pass

    def _on_backoff_updated(self, remaining: int) -> None:
        if remaining > 0:
            self.backoff_label.setText(f"Cooling down {remaining}s to avoid rate limits…")
        else:
            self.backoff_label.clear()

    def _on_rate_limit_notice(self, message: str) -> None:
        msg = (message or "").strip()
        if msg:
            self.backoff_label.setText(msg)
        else:
            self.backoff_label.clear()

    def _on_parallel_changed(self, eff: int) -> None:
        self.adapt_label.setText(f"Adaptive parallel: {eff}")

    def _on_command_line(self, job_id: int, item_id: int, cmd: str) -> None:
        if platform.system() == "Windows" and self._read_bool(KEYS["persistent_terminal"], False):
            self._ensure_persistent_terminal(start_hidden=True)
            self._send_to_persistent(cmd)
        self._last_cmd = cmd

    # ------------------------------------------------------------------
    # Job maintenance helpers
    # ------------------------------------------------------------------
    def _retry_failed_items(self) -> None:
        item = self.job_list.currentItem()
        if not item:
            QMessageBox.information(self, "Select job", "Select a job first.")
            return
        job_id = self._job_id_from_item(item)
        job = self.job_queue.get_job(job_id)
        if not job:
            return
        if job.state == JobState.RUNNING:
            QMessageBox.warning(self, "Busy", "Cannot retry while job is running.")
            return
        changed = 0
        for it in job.items:
            if it.state in {JobItemState.FAILED, JobItemState.CANCELLED}:
                self.job_queue.set_item_state(job.job_id, it.item_id, state=JobItemState.PENDING, progress=0, error="")
                changed += 1
        if changed:
            self._notify("Retry", f"Reset {changed} item(s).", 2500)

    def _remove_completed_jobs(self) -> None:
        removed = 0
        for job in list(self.job_queue.jobs()):
            if job.state in {JobState.SUCCESS, JobState.CANCELLED}:
                self.job_queue.remove_job(job.job_id)
                removed += 1
        if removed:
            self._notify("Queue cleaned", f"Removed {removed} completed job(s).", 2500)

    # ------------------------------------------------------------------
    # History
    # ------------------------------------------------------------------
    def _load_history(self) -> List[Dict]:
        try:
            return json.loads(self.s.value(KEYS["history"], "[]"))
        except Exception:
            return []

    def _save_history(self, hist: List[Dict]) -> None:
        try:
            cap = self._read_int(KEYS.get("history_max", "history_max"), 100)
            cap = max(10, cap)
            self.s.setValue(KEYS["history"], json.dumps(hist[-cap:]))
        except Exception:
            pass

    def _append_history(self, result) -> None:
        hist = self._load_history()
        job = result.job
        entry = {
            "start_iso": result.started_iso,
            "finished_iso": result.finished_iso,
            "code": 0 if result.state == JobState.SUCCESS else 1,
            "dest": RunOptions.from_payload(job.options).dest,
            "log_path": result.items[-1].log_path if result.items else "",
            "input": job.label,
            "urls": len(job.items),
            "moved": result.totals.moved,
            "replaced": result.totals.replaced,
            "deleted": result.totals.deleted,
            "skipped": result.totals.skipped,
            "suspect": result.totals.suspect,
        }
        hist.append(entry)
        self._save_history(hist)

    # ------------------------------------------------------------------
    # Settings / dialogs
    # ------------------------------------------------------------------
    def open_settings(self) -> None:
        dlg = SettingsDialog(self)
        if dlg.exec():
            if platform.system() == "Windows" and self._read_bool(KEYS["persistent_terminal"], False):
                self._ensure_persistent_terminal(start_hidden=True)
            self._update_bin_pill()
            self._sentry_enabled = self._read_bool(KEYS.get("sentry_enabled", "sentry_enabled"), False)
            try:
                self._sentry_gap_sec = max(25, int(self.s.value(KEYS.get("sentry_gap_sec", "sentry_gap_sec"), 25)))
            except Exception:
                self._sentry_gap_sec = 25
            self._update_sentry_indicator()

    def open_history(self) -> None:
        dlg = HistoryDialog(self, self._load_history())
        dlg.sig_requeue.connect(lambda urls: self._create_job_from_urls(urls, QueueSource.MANUAL))
        dlg.exec()

    def open_shortcuts(self) -> None:
        entries = getattr(self, "_shortcuts_list", [])
        dlg = ShortcutsDialog(self, entries)
        dlg.exec()
    # ------------------------------------------------------------------
    # Clipboard watcher / scheduler
    # ------------------------------------------------------------------
    def _tick_clipboard(self) -> None:
        txt = QApplication.clipboard().text().strip()
        if not txt or txt == getattr(self, "_last_clip", ""):
            return
        self._last_clip = txt
        if not (self.auto_clip.isChecked() or getattr(self, "_sentry_enabled", False)):
            return
        candidates = [s for s in re.split(r"[\s\r\n]+", txt) if s]
        urls = [u for u in candidates if SPOTIFY_URL_RE.match(u)]
        if not urls:
            return
        if getattr(self, "_sentry_enabled", False):
            hist_inputs = {h.get("input") for h in self._load_history() if int(h.get("code", -1)) == 0}
            urls = [u for u in urls if u not in hist_inputs]
            if not urls:
                return
        ok, msg = self._create_job_from_urls(urls, QueueSource.SENTRY, auto_run=self._sentry_enabled)
        if ok:
            self._notify("Clipboard captured", msg, 2500)

    def _tick_scheduler(self) -> None:
        if not self._read_bool(KEYS.get("scheduler_enabled", "scheduler_enabled"), False):
            return
        sched_time = self.s.value(KEYS.get("scheduler_time", "scheduler_time"), "00:00")
        try:
            target = QTime.fromString(str(sched_time), "HH:mm")
        except Exception:
            return
        now = QTime.currentTime()
        if now.hour() == target.hour() and abs(now.minute() - target.minute()) <= 1:
            if not self.runner.is_running():
                job = self.job_queue.next_pending_job()
                if job:
                    self._start_job(job)
    # ------------------------------------------------------------------
    # Web queue server
    # ------------------------------------------------------------------
    @Slot(str, str, result=object)
    def queue_from_web(self, urls_json: str, dest_override: str):
        try:
            urls = json.loads(urls_json)
        except Exception:
            urls = []
        if not isinstance(urls, list):
            urls = []
        return self.handle_web_submission([str(u) for u in urls], dest_override, source=QueueSource.WEB)

    def handle_web_submission(self, urls: List[str], dest_override: str, source: QueueSource | None = QueueSource.WEB) -> Tuple[bool, str, List[str]]:
        queue_source = source or QueueSource.WEB
        ok, msg = self._create_job_from_urls(urls, queue_source, dest_override=dest_override, auto_run=True)
        return ok, msg, [] if ok else urls

    def _stop_web_server(self) -> None:
        if self._web_server:
            try:
                self._web_server.stop()
            except Exception:
                pass
            self._web_server = None

    def _configure_web_server(self, force: bool = False) -> None:
        try:
            self.s.sync()
        except Exception:
            pass
        should_run = self._read_bool(KEYS.get("web_enabled", "web_enabled"), False)
        if not should_run:
            if self._web_server:
                self._notify('Web server', 'Remote queue server stopped.', 2500)
            self._stop_web_server()
            return

        host = (self.s.value(KEYS.get("web_host", "web_host"), "127.0.0.1") or "127.0.0.1").strip()
        try:
            port = int(self.s.value(KEYS.get("web_port", "web_port"), 9753))
        except Exception:
            port = 9753
        username = (self.s.value(KEYS.get("web_username", "web_username"), "") or '').strip()
        password = (self.s.value(KEYS.get("web_password", "web_password"), "") or '')
        dest_override = (self.s.value(KEYS.get("web_dest_override", "web_dest_override"), "") or '').strip()

        server = self._web_server
        settings_changed = bool(
            server and (
                server.host != host
                or server.port != port
                or server.username != username
                or server.password != password
                or server.dest_override != dest_override
            )
        )

        if force or settings_changed:
            self._stop_web_server()
            server = None

        if not server:
            server = WebQueueServer(self, host, port, username, password, dest_override)
            ok, msg = server.start()
            if ok:
                self._web_server = server
                self._notify('Web server', msg, 3500)
            else:
                self._notify('Web server', f'Failed to start: {msg}', 6000)
        else:
            self._web_server = server

    def get_web_status(self) -> Dict:
        return {
            'queue_size': self.job_list.count(),
            'is_running': self.runner.is_running(),
            'active_job': self._current_job_id,
            'last_run': getattr(self, '_last_run_summary', 'Never'),
        }
    # ------------------------------------------------------------------
    # Notifications / tray
    # ------------------------------------------------------------------
    def _notify(self, title: str, body: str, ms: int = 4000) -> None:
        if self.tray:
            self.tray.showMessage(title, body, QSystemTrayIcon.Information, ms)

    def _tray_show(self) -> None:
        self.showNormal()
        self.raise_()
        self.activateWindow()
        if platform.system() == "Windows":
            hwnd = console_hwnd_for_pid(os.getpid())
            show_window(hwnd)

    def _quit(self) -> None:
        if self.runner.is_running():
            if not self._standard_confirm("Quit", "A download is in progress. Quit and stop it?"):
                return
            self.runner.stop()
        self._really_quit = True
        self._stop_web_server()
        self.close()

    def closeEvent(self, event) -> None:  # noqa: N802
        if self._read_bool(KEYS["minimize_to_tray"], True) and self.tray and not self._really_quit:
            event.ignore()
            self.hide()
            self.tray.showMessage(APP_NAME, "Still running in tray…", QSystemTrayIcon.Information, 2500)
            return
        super().closeEvent(event)
    # ------------------------------------------------------------------
    # Misc helpers
    # ------------------------------------------------------------------
    def _update_bin_pill(self) -> None:
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
        self.bin_pill.setStyleSheet(
            f"""
            QLabel#binPill {{
                background: {bg};
                color: {color};
                border: 1px solid {border};
                border-radius: 999px;
                padding: 6px 10px;
            }}
            """
        )

    def _get_spotifydl_version(self, exe_path: str) -> Optional[str]:
        try:
            out = subprocess.run([exe_path, "--version"], capture_output=True, text=True, timeout=2)
            txt = (out.stdout or out.stderr or "").strip()
            return txt.splitlines()[0].strip() if txt else None
        except Exception:
            return None

    def _update_sentry_indicator(self) -> None:
        if getattr(self, "_sentry_enabled", False):
            gap = max(25, int(getattr(self, "_sentry_gap_sec", 25)))
            self._sentry_label.setText(f'Sentry: ON  -  gap {gap}s')
        else:
            self._sentry_label.setText('Sentry: OFF')

    def _toggle_sentry(self, enabled: bool) -> None:
        self._sentry_enabled = bool(enabled)
        self.s.setValue(KEYS.get("sentry_enabled", "sentry_enabled"), "true" if enabled else "false")
        self._update_sentry_indicator()

    def _open_path(self, path: str) -> None:
        try:
            if sys.platform.startswith("darwin"):
                subprocess.call(["open", path])
            elif os.name == "nt":
                os.startfile(path)  # type: ignore[attr-defined]
            else:
                subprocess.call(["xdg-open", path])
        except Exception:
            pass

    def _update_taskbar_progress(self, pct: int) -> None:
        # Placeholder: real taskbar integration can be added later.
        pass

    def _update_tray_tooltip(self, pct: int) -> None:
        if not self.tray:
            return
        if pct >= 100:
            self.tray.setToolTip("Idle")
        else:
            self.tray.setToolTip(f"Running… {pct}%")

    def _ensure_persistent_terminal(self, start_hidden: bool = False) -> None:
        if self._persist_proc and self._persist_proc.poll() is None:
            return
        if platform.system() != "Windows":
            return
        script = Path(sys.argv[0]).resolve()
        cmd = ["cmd.exe", "/c", f"start {'/min ' if start_hidden else ''}cmd.exe /k python \"{script}\""]
        try:
            self._persist_proc = subprocess.Popen(cmd, creationflags=subprocess.CREATE_NEW_CONSOLE)  # type: ignore[attr-defined]
        except Exception:
            self._persist_proc = None

    def _send_to_persistent(self, text: str) -> None:
        # Future enhancement: send commands to persistent console via named pipe or socket.
        pass

    def _install_shortcuts(self) -> None:
        from PySide6.QtGui import QAction, QKeySequence

        specs = [
            ("Ctrl+R", "Run queue", self._run),
            ("Ctrl+P", "Pause/Resume queue", self._pause_toggle),
            ("Ctrl+S", "Stop", self._stop),
            ("Delete", "Remove selected job", self._remove_selected_jobs),
            ("Ctrl+I", "Import queue", self._import_queue),
            ("Ctrl+E", "Export queue", self._export_queue),
            ("Ctrl+L", "Focus links area", lambda: self.staging.setFocus()),
            ("Ctrl+Return", "Add URLs from links area", self._add_from_staging),
            ("Ctrl+D", "Focus destination field", lambda: self.dest.setFocus()),
            ("Ctrl+B", "Browse destination folder", self._pick_dest),
            ("Alt+Up", "Move job up", lambda: self._nudge_job(-1)),
            ("Alt+Down", "Move job down", lambda: self._nudge_job(+1)),
            ("Ctrl+H", "Open history", self.open_history),
            ("Ctrl+,", "Open settings", self.open_settings),
            ("F1", "Show shortcuts", self.open_shortcuts),
            ("Ctrl+Q", "Quit", self._quit),
        ]

        self._actions: List[QAction] = []
        for keys, _desc, slot in specs:
            act = QAction(self)
            act.setShortcut(QKeySequence(keys))
            act.triggered.connect(slot)
            act.setShortcutVisibleInContextMenu(False)
            self.addAction(act)
            self._actions.append(act)
        self._shortcuts_list = [(k, d) for k, d, _ in specs]
    # ------------------------------------------------------------------
    # Import / Export
    # ------------------------------------------------------------------
    def _import_queue(self) -> None:
        path, _ = QFileDialog.getOpenFileName(self, "Import queue", "", "Queue files (*.json *.txt);;All files (*)")
        if not path:
            return
        try:
            text = Path(path).read_text(encoding="utf-8", errors="ignore")
            if path.lower().endswith(".json"):
                data = json.loads(text)
                if isinstance(data, dict) and "jobs" in data:
                    jobs = data.get("jobs", [])
                    for entry in jobs:
                        urls = entry.get("urls", []) if isinstance(entry, dict) else []
                        if isinstance(urls, list):
                            self._create_job_from_urls(urls, QueueSource.MANUAL)
                else:
                    urls = data.get("urls", []) if isinstance(data, dict) else data
                    if isinstance(urls, list):
                        self._create_job_from_urls(urls, QueueSource.MANUAL)
            else:
                urls = [ln.strip() for ln in text.splitlines() if ln.strip()]
                if urls:
                    self._create_job_from_urls(urls, QueueSource.MANUAL)
            self._notify("Queue imported", "Jobs added from file.", 2500)
        except Exception as exc:
            QMessageBox.critical(self, "Import failed", str(exc))

    def _export_queue(self) -> None:
        if not self.job_list.count():
            QMessageBox.information(self, "Nothing to export", "Queue is empty.")
            return
        path, _ = QFileDialog.getSaveFileName(self, "Export queue", "queue.json", "JSON (*.json);;Text (*.txt)")
        if not path:
            return
        try:
            jobs = []
            for job in self.job_queue.jobs():
                jobs.append({"label": job.label, "urls": [it.url for it in job.items], "state": job.state.value})
            if path.lower().endswith(".json"):
                Path(path).write_text(json.dumps({"jobs": jobs}, indent=2), encoding="utf-8")
            else:
                lines: List[str] = []
                for job in jobs:
                    lines.append(f"# {job['label']}")
                    lines.extend(job["urls"])
                    lines.append("")
                Path(path).write_text("\n".join(lines), encoding="utf-8")
            self._notify("Queue exported", f"{len(jobs)} job(s) saved.", 2500)
        except Exception as exc:
            QMessageBox.critical(self, "Export failed", str(exc))

    def _export_single_job(self, job_id: int) -> None:
        job = self.job_queue.get_job(job_id)
        if not job:
            return
        path, _ = QFileDialog.getSaveFileName(self, f"Export job {job.label}", f"{job.label}.txt", "Text (*.txt)")
        if not path:
            return
        try:
            Path(path).write_text("\n".join([it.url for it in job.items]), encoding="utf-8")
            self._notify("Job exported", f"{len(job.items)} item(s) saved.", 2500)
        except Exception as exc:
            QMessageBox.critical(self, "Export failed", str(exc))
