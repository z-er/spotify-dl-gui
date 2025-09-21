# spotifydl_gui/ui/history_dialog.py
"""
History viewer dialog.

Displays past job summaries with quick actions:
- Open selected log file
- Open destination folder
- Filter by text (artist / album / URL / status)
"""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import List, Dict

from PySide6.QtCore import Qt
from PySide6.QtWidgets import (
    QDialog, QVBoxLayout, QHBoxLayout, QListWidget, QListWidgetItem,
    QPushButton, QLineEdit, QLabel, QDialogButtonBox, QMessageBox
)


def _open_path(path: str) -> None:
    try:
        if sys.platform.startswith("win"):
            os.startfile(path)  # type: ignore[attr-defined]
        elif sys.platform == "darwin":
            os.spawnlp(os.P_NOWAIT, "open", "open", path)
        else:
            os.spawnlp(os.P_NOWAIT, "xdg-open", "xdg-open", path)
    except Exception:
        pass


def _reveal_in_folder(path: str) -> None:
    p = Path(path)
    try:
        if sys.platform.startswith("win"):
            # Show in Explorer with selection
            if p.exists():
                os.spawnl(os.P_NOWAIT, "C:\\Windows\\explorer.exe", "explorer", f'/select,"{str(p)}"')
            else:
                os.startfile(str(p.parent))  # type: ignore[attr-defined]
        elif sys.platform == "darwin":
            if p.exists():
                os.spawnlp(os.P_NOWAIT, "open", "open", "-R", str(p))
            else:
                os.spawnlp(os.P_NOWAIT, "open", "open", str(p.parent))
        else:
            # Linux: best effort — open folder
            _open_path(str(p.parent))
    except Exception:
        _open_path(str(p.parent))


class HistoryDialog(QDialog):
    def __init__(self, parent, history: List[Dict]):
        super().__init__(parent)
        self.setWindowTitle("History")
        self.setMinimumSize(760, 520)
        self._all = history or []

        # Header + filter
        title = QLabel("Previous runs"); title.setStyleSheet("font-size: 15px; font-weight: 600;")
        self.filter_edit = QLineEdit(); self.filter_edit.setPlaceholderText("Filter by artist, album, URL, or status…")
        self.filter_edit.textChanged.connect(self._apply_filter)

        header = QHBoxLayout()
        header.addWidget(title); header.addStretch(); header.addWidget(self.filter_edit)

        # List
        self.list = QListWidget()
        self._populate(self._all)

        # Buttons
        self.btn_open_log = QPushButton("Open log")
        self.btn_open_log.clicked.connect(self.open_log)
        self.btn_reveal_log = QPushButton("Reveal log in folder")
        self.btn_reveal_log.clicked.connect(self.reveal_log)
        self.btn_open_dest = QPushButton("Open destination")
        self.btn_open_dest.clicked.connect(self.open_dest)

        actions = QHBoxLayout()
        actions.addWidget(self.btn_open_log)
        actions.addWidget(self.btn_reveal_log)
        actions.addSpacing(8)
        actions.addWidget(self.btn_open_dest)
        actions.addStretch()

        btns = QDialogButtonBox(QDialogButtonBox.Close)
        btns.rejected.connect(self.reject)
        btns.accepted.connect(self.accept)
        btns.button(QDialogButtonBox.Close).clicked.connect(self.close)

        # Layout
        layout = QVBoxLayout(self)
        layout.addLayout(header)
        layout.addWidget(self.list, 1)
        layout.addLayout(actions)
        layout.addWidget(btns)

    # -------- internal helpers --------
    def _populate(self, jobs: List[Dict]) -> None:
        self.list.clear()
        for job in jobs:
            ts = job.get("start_iso", "?")
            dest = job.get("dest", "")
            code = int(job.get("code", -1))
            moved = job.get("moved", 0)
            replaced = job.get("replaced", 0)
            deleted = job.get("deleted", 0)
            skipped = job.get("skipped", 0)
            suspect = job.get("suspect", 0)
            artist = job.get("first_artist", "")
            album = job.get("first_album", "")
            url = job.get("input", "")

            status = "OK ✅" if code == 0 else f"Exit {code} ❌"
            label = f"[{ts}]  {status}  → {dest}"
            if artist or album:
                label += f"   ({artist} – {album})"
            label += f"   (moved:{moved} repl:{replaced} del:{deleted} skip:{skipped}"
            if suspect:
                label += f" suspect:{suspect}"
            label += ")"
            if url:
                label += f"\n{url}"

            item = QListWidgetItem(label)
            item.setData(Qt.UserRole, job)
            self.list.addItem(item)

    def _selected_job(self) -> Dict | None:
        it = self.list.currentItem()
        return it.data(Qt.UserRole) if it else None

    def _apply_filter(self, text: str) -> None:
        t = (text or "").lower().strip()
        if not t:
            self._populate(self._all)
            return
        filtered = []
        for job in self._all:
            fields = [
                job.get("first_artist", ""), job.get("first_album", ""),
                job.get("input", ""), job.get("dest", ""), job.get("start_iso", ""),
                "ok" if int(job.get("code", -1)) == 0 else "fail",
            ]
            joined = " ".join(f for f in fields if f).lower()
            if t in joined:
                filtered.append(job)
        self._populate(filtered)

    # -------- actions --------
    def open_log(self):
        job = self._selected_job()
        if not job:
            return
        p = job.get("log_path", "")
        if not p or not Path(p).exists():
            QMessageBox.information(self, "No log", "The selected run has no saved log.")
            return
        _open_path(p)

    def reveal_log(self):
        job = self._selected_job()
        if not job:
            return
        p = job.get("log_path", "")
        if not p:
            return
        _reveal_in_folder(p)

    def open_dest(self):
        job = self._selected_job()
        if not job:
            return
        d = job.get("dest", "")
        if not d or not Path(d).exists():
            QMessageBox.information(self, "Missing folder", "Destination folder no longer exists.")
            return
        _open_path(d)
