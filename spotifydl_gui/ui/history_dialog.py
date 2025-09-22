# spotifydl_gui/ui/history_dialog.py
"""
History viewer dialog (enhanced).

Adds:
- Search box (artist / album / URL / dest)
- Status filter: All / OK / Failed / Has Suspects
- Stats header (jobs, ok, fail, suspects, total size if log contains it)
- "Re-queue selected" button that emits the original URLs of selected jobs

Assumptions:
- Each history entry is a dict like we store in MainWindow._append_history(...)
- If log_path exists and contains a JSON summary, we try to compute total size.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import List, Dict, Tuple

from PySide6.QtCore import Qt, Signal
from PySide6.QtWidgets import (
    QDialog, QVBoxLayout, QHBoxLayout, QListWidget, QListWidgetItem,
    QPushButton, QLineEdit, QLabel, QDialogButtonBox, QMessageBox, QComboBox,
    QFileDialog
)
from ..settings_store import KEYS


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
            _open_path(str(p.parent))
    except Exception:
        _open_path(str(p.parent))


class HistoryDialog(QDialog):
    # Emits a list of original input URLs to re-queue
    sig_requeue = Signal(list)

    def __init__(self, parent, history: List[Dict]):
        super().__init__(parent)
        self.setWindowTitle("History")
        self.setMinimumSize(820, 560)
        self._all = history or []
        self._visible: List[Dict] = list(self._all)

        # ----- Header: title + search + status filter -----
        title = QLabel("Previous runs"); title.setStyleSheet("font-size: 15px; font-weight: 600;")
        self.filter_edit = QLineEdit(); self.filter_edit.setPlaceholderText("Search artist, album, URL, or destination…")
        self.filter_edit.textChanged.connect(self._apply_filter)

        self.status_combo = QComboBox()
        self.status_combo.addItems(["All", "OK", "Failed", "Has suspects"])
        self.status_combo.currentIndexChanged.connect(self._apply_filter)

        header = QHBoxLayout()
        header.addWidget(title)
        header.addStretch()
        header.addWidget(QLabel("Status"))
        header.addWidget(self.status_combo)
        header.addWidget(self.filter_edit)

        # ----- Stats bar -----
        self.stats_lbl = QLabel("")
        self.stats_lbl.setProperty("class", "muted")

        # ----- List -----
        self.list = QListWidget()
        self.list.setSelectionMode(QListWidget.ExtendedSelection)

        # ----- Actions -----
        self.btn_open_log = QPushButton("Open log")
        self.btn_open_log.clicked.connect(self.open_log)
        self.btn_reveal_log = QPushButton("Reveal log in folder")
        self.btn_reveal_log.clicked.connect(self.reveal_log)
        self.btn_open_dest = QPushButton("Open destination")
        self.btn_open_dest.clicked.connect(self.open_dest)

        self.btn_requeue = QPushButton("Re-queue selected")
        self.btn_requeue.setToolTip("Add the original input URL(s) back to the main queue.")
        self.btn_requeue.clicked.connect(self.requeue_selected)

        self.btn_export = QPushButton("Export visible…")
        self.btn_export.setToolTip("Export the currently visible rows to a JSON file.")
        self.btn_export.clicked.connect(self.export_visible)

        self.btn_clear = QPushButton("Clear history")
        self.btn_clear.setToolTip("Erase all stored history entries (this cannot be undone).")
        self.btn_clear.clicked.connect(self.clear_history)

        actions = QHBoxLayout()
        actions.addWidget(self.btn_open_log)
        actions.addWidget(self.btn_reveal_log)
        actions.addSpacing(8)
        actions.addWidget(self.btn_open_dest)
        actions.addStretch()
        actions.addWidget(self.btn_export)
        actions.addWidget(self.btn_requeue)
        actions.addWidget(self.btn_clear)

        btns = QDialogButtonBox(QDialogButtonBox.Close)
        btns.rejected.connect(self.reject)
        btns.accepted.connect(self.accept)
        btns.button(QDialogButtonBox.Close).clicked.connect(self.close)

        # ----- Layout -----
        layout = QVBoxLayout(self)
        layout.addLayout(header)
        layout.addWidget(self.stats_lbl)
        layout.addWidget(self.list, 1)
        layout.addLayout(actions)
        layout.addWidget(btns)

        # Bootstrap view
        self._refresh()

    # -------- internal helpers --------
    def _refresh(self):
        self._apply_filter(update_list_only=False)

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

    def _selected_jobs(self) -> List[Dict]:
        items = self.list.selectedItems()
        return [it.data(Qt.UserRole) for it in items] if items else []

    def _apply_filter(self, *_args, update_list_only: bool = True) -> None:
        # Text filter
        t = (self.filter_edit.text() or "").lower().strip()
        # Status filter
        mode = self.status_combo.currentText()

        filtered = []
        for job in self._all:
            # status logic
            code = int(job.get("code", -1))
            suspect = int(job.get("suspect", 0))
            if mode == "OK" and code != 0:
                continue
            if mode == "Failed" and code == 0:
                continue
            if mode == "Has suspects" and suspect <= 0:
                continue

            if not t:
                filtered.append(job); continue

            fields = [
                job.get("first_artist", ""), job.get("first_album", ""),
                job.get("input", ""), job.get("dest", ""), job.get("start_iso", ""),
            ]
            joined = " ".join(f for f in fields if f).lower()
            if t in joined:
                filtered.append(job)

        self._visible = filtered
        self._populate(self._visible)

        # Recompute stats if requested
        if not update_list_only:
            jobs_stats, total_size = self._compute_stats(self._all)
            ok = jobs_stats["ok"]; fail = jobs_stats["fail"]; sus = jobs_stats["suspects"]
            if total_size is None:
                size_text = ""
            else:
                size_text = f" • Size: {self._fmt_size(total_size)}"
            self.stats_lbl.setText(
                f"Jobs: {len(self._all)} • OK: {ok} • Fail: {fail} • With suspects: {sus}{size_text}"
            )

    # -------- stats helpers --------
    def _compute_stats(self, jobs: List[Dict]) -> Tuple[Dict[str, int], int | None]:
        ok = fail = sus = 0
        total_size: int | None = 0
        for j in jobs:
            if int(j.get("code", -1)) == 0:
                ok += 1
            else:
                fail += 1
            if int(j.get("suspect", 0)) > 0:
                sus += 1

            # Try to read output sizes from log JSON
            lp = j.get("log_path", "")
            if not lp or not Path(lp).exists():
                total_size = None if total_size == 0 else total_size
                continue
            try:
                # Read tail of file to find JSON summary (we wrote it after '=== Summary (JSON) ===')
                txt = Path(lp).read_text(encoding="utf-8", errors="ignore")
                sep = "\n=== Summary (JSON) ===\n"
                if sep in txt:
                    js = txt.split(sep, 1)[1]
                    data = json.loads(js)
                    outs = data.get("outputs", [])
                    for o in outs:
                        sz = int(o.get("size", 0))
                        if total_size is not None:
                            total_size += max(0, sz)
                else:
                    total_size = None if total_size == 0 else total_size
            except Exception:
                total_size = None if total_size == 0 else total_size

        return {"ok": ok, "fail": fail, "suspects": sus}, total_size

    @staticmethod
    def _fmt_size(n: int) -> str:
        # bytes -> human
        step = 1024.0
        units = ["B", "KB", "MB", "GB", "TB"]
        s = float(n)
        for u in units:
            if s < step:
                return f"{s:.2f} {u}"
            s /= step
        return f"{s:.2f} PB"

    # -------- actions --------
    def open_log(self):
        job = self._selected_job_single()
        if not job:
            return
        p = job.get("log_path", "")
        if not p or not Path(p).exists():
            QMessageBox.information(self, "No log", "The selected run has no saved log.")
            return
        _open_path(p)

    def reveal_log(self):
        job = self._selected_job_single()
        if not job:
            return
        p = job.get("log_path", "")
        if not p:
            return
        _reveal_in_folder(p)

    def open_dest(self):
        job = self._selected_job_single()
        if not job:
            return
        d = job.get("dest", "")
        if not d or not Path(d).exists():
            QMessageBox.information(self, "Missing folder", "Destination folder no longer exists.")
            return
        _open_path(d)

    def requeue_selected(self):
        jobs = self._selected_jobs()
        if not jobs:
            QMessageBox.information(self, "Nothing selected", "Select one or more runs to re-queue.")
            return
        urls = []
        for j in jobs:
            u = (j.get("input") or "").strip()
            if u:
                urls.append(u)
        if not urls:
            QMessageBox.information(self, "No URLs", "Selected runs contain no input URLs.")
            return
        self.sig_requeue.emit(urls)

    def export_visible(self):
        if not self._visible:
            QMessageBox.information(self, "Nothing to export", "No entries match the current filter.")
            return
        path, _ = QFileDialog.getSaveFileName(self, "Export history", "history.json", "JSON (*.json)")
        if not path:
            return
        try:
            Path(path).write_text(json.dumps(self._visible, indent=2, ensure_ascii=False), encoding="utf-8")
            QMessageBox.information(self, "Exported", f"Saved {len(self._visible)} entries to:\n{path}")
        except Exception as e:
            QMessageBox.critical(self, "Export failed", str(e))

    def clear_history(self):
        if QMessageBox.question(
            self, "Clear history",
            "This will remove ALL saved history entries.\n\nProceed?",
            QMessageBox.Yes | QMessageBox.No, QMessageBox.No
        ) != QMessageBox.Yes:
            return
        try:
            parent = self.parent()
            settings = getattr(parent, "s", None)
            if settings is not None:
                settings.setValue(KEYS["history"], "[]")
        except Exception:
            pass
        self._all = []
        self._visible = []
        self._populate(self._visible)
        self.stats_lbl.setText("")
        QMessageBox.information(self, "History cleared", "All history entries have been removed.")

    # -------- list helpers --------
    def _selected_job_single(self) -> Dict | None:
        it = self.list.currentItem()
        return it.data(Qt.UserRole) if it else None
