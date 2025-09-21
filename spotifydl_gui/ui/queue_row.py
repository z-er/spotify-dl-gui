# spotifydl_gui/ui/queue_row.py
"""
QueueRow widget: a compact row showing
- URL (selectable)
- status badge + label
- right-aligned progress bar

Used inside QListWidget via setItemWidget(item, QueueRow(...)).
MainWindow updates rows via set_status(...) and set_progress(...).
"""

from __future__ import annotations

from enum import Enum
from PySide6.QtCore import Qt
from PySide6.QtWidgets import QWidget, QLabel, QHBoxLayout, QProgressBar


class QStatus(str, Enum):
    PENDING = "Pending"
    RUNNING = "Running"
    OK      = "Done"
    FAIL    = "Failed"
    PAUSED  = "Paused"
    SKIPPED = "Skipped"


_BADGE = {
    QStatus.PENDING: "⏳",
    QStatus.RUNNING: "▶️",
    QStatus.OK:      "✅",
    QStatus.FAIL:    "❌",
    QStatus.PAUSED:  "⏸️",
    QStatus.SKIPPED: "⤼",
}

_STATUS_COLOR = {
    QStatus.PENDING: "#b5bcc9",
    QStatus.RUNNING: "#f4a261",
    QStatus.OK:      "#8ad7a0",
    QStatus.FAIL:    "#f7768e",
    QStatus.PAUSED:  "#f4a261",
    QStatus.SKIPPED: "#b5bcc9",
}


class QueueRow(QWidget):
    """
    Lightweight row widget to embed inside QListWidget.
    """

    def __init__(self, url: str, status: QStatus = QStatus.PENDING, parent=None):
        super().__init__(parent)
        self._url = url
        self._status = status

        self.lbl_badge = QLabel(_BADGE[status]); self.lbl_badge.setFixedWidth(24)
        self.lbl_url = QLabel(url)
        self.lbl_url.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.lbl_url.setWordWrap(False)
        self.lbl_url.setMinimumWidth(240)
        self.lbl_url.setToolTip(url)
        self.lbl_status = QLabel(status.value)
        self.lbl_status.setStyleSheet(f"color: {_STATUS_COLOR[status]};")

        self.progress = QProgressBar()
        self.progress.setRange(0, 100)
        self.progress.setValue(0)
        self.progress.setFixedWidth(120)
        self.progress.setTextVisible(True)

        row = QHBoxLayout(self)
        row.setContentsMargins(8, 6, 8, 6)
        row.setSpacing(10)
        row.addWidget(self.lbl_badge)
        row.addWidget(self.lbl_url, 1)
        row.addWidget(self.lbl_status)
        row.addWidget(self.progress)

        # Subtle card styling (works with our global dark theme)
        self.setStyleSheet("""
            QWidget {
                background: rgba(255,255,255,0.02);
                border: 1px solid #2a2f39;
                border-radius: 10px;
            }
            QProgressBar {
                border: 1px solid #2a2f39;
                border-radius: 8px;
                text-align: center;
                background: #1a212b;
                height: 16px;
            }
            QProgressBar::chunk {
                background-color: #f4a261;
                border-radius: 8px;
            }
        """)

    # -------- API ----------
    def url(self) -> str:
        return self._url

    def status(self) -> QStatus:
        return self._status

    def set_status(self, status: QStatus) -> None:
        self._status = status
        self.lbl_badge.setText(_BADGE[status])
        self.lbl_status.setText(status.value)
        self.lbl_status.setStyleSheet(f"color: {_STATUS_COLOR[status]};")
        if status in (QStatus.OK, QStatus.FAIL, QStatus.SKIPPED):
            self.progress.setValue(100 if status == QStatus.OK else 0)

    def set_progress(self, pct: int) -> None:
        pct = max(0, min(100, int(pct)))
        self.progress.setValue(pct)

    # Convenience for QListWidget hooks
    def set_selected(self, selected: bool) -> None:
        # visual tweak hook if needed (currently no-op because selection is handled by QListWidget)
        pass
