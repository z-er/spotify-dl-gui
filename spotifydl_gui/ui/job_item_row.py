"""UI widget for individual Job items within a Job."""

from __future__ import annotations

from PySide6.QtCore import Qt
from PySide6.QtWidgets import QLabel, QHBoxLayout, QProgressBar, QWidget

from ..job_types import JobItem, JobItemState


_STATE_BADGE = {
    JobItemState.PENDING: "🕒",
    JobItemState.RUNNING: "▶️",
    JobItemState.SUCCESS: "✅",
    JobItemState.FAILED: "❌",
    JobItemState.SKIPPED: "⤼",
    JobItemState.CANCELLED: "🚫",
}

_STATE_COLOR = {
    JobItemState.PENDING: "#b5bcc9",
    JobItemState.RUNNING: "#f4a261",
    JobItemState.SUCCESS: "#8ad7a0",
    JobItemState.FAILED: "#f7768e",
    JobItemState.SKIPPED: "#b5bcc9",
    JobItemState.CANCELLED: "#f7768e",
}


class JobItemRow(QWidget):
    def __init__(self, item: JobItem, parent=None):
        super().__init__(parent)
        self._item_id = item.item_id

        self.lbl_badge = QLabel("🕒")
        self.lbl_badge.setFixedWidth(24)

        self.lbl_url = QLabel()
        self.lbl_url.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.lbl_url.setWordWrap(False)
        self.lbl_url.setMinimumWidth(260)

        self.lbl_status = QLabel()

        self.progress = QProgressBar()
        self.progress.setRange(0, 100)
        self.progress.setFixedWidth(120)
        self.progress.setTextVisible(True)

        lay = QHBoxLayout(self)
        lay.setContentsMargins(8, 6, 8, 6)
        lay.setSpacing(10)
        lay.addWidget(self.lbl_badge)
        lay.addWidget(self.lbl_url, 1)
        lay.addWidget(self.lbl_status)
        lay.addWidget(self.progress)

        self.setStyleSheet(
            """
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
            """
        )

        self.update_from_item(item)

    @property
    def item_id(self) -> int:
        return self._item_id

    def update_from_item(self, item: JobItem) -> None:
        self._item_id = item.item_id
        state = item.state
        badge = _STATE_BADGE.get(state, "")
        color = _STATE_COLOR.get(state, "#b5bcc9")
        self.lbl_badge.setText(badge)
        self.lbl_url.setText(item.url)
        self.lbl_url.setToolTip(item.url)
        self.lbl_status.setText(state.value.title())
        self.lbl_status.setStyleSheet(f"color: {color};")
        self.progress.setValue(max(0, min(100, int(item.progress))))

    def set_selected(self, selected: bool) -> None:
        # placeholder hook for selection styling if needed later
        pass
