"""UI widget summarising a Job in the master queue."""

from __future__ import annotations

from PySide6.QtCore import Qt
from PySide6.QtWidgets import QLabel, QHBoxLayout, QProgressBar, QWidget

from ..job_types import Job, JobItemState, JobState


_STATE_BADGE = {
    JobState.PENDING: "🕒",
    JobState.RUNNING: "▶️",
    JobState.PAUSED: "⏸️",
    JobState.SUCCESS: "✅",
    JobState.FAILED: "❌",
    JobState.CANCELLED: "🚫",
}

_STATE_COLOR = {
    JobState.PENDING: "#b5bcc9",
    JobState.RUNNING: "#f4a261",
    JobState.PAUSED: "#f4a261",
    JobState.SUCCESS: "#8ad7a0",
    JobState.FAILED: "#f7768e",
    JobState.CANCELLED: "#f7768e",
}


class JobRow(QWidget):
    """Compact widget showing a job summary for the QListWidget."""

    def __init__(self, job: Job, parent=None):
        super().__init__(parent)
        self._job_id = job.job_id

        self.lbl_badge = QLabel()
        self.lbl_badge.setFixedWidth(28)

        self.lbl_title = QLabel()
        self.lbl_title.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.lbl_title.setWordWrap(False)
        self.lbl_title.setMinimumWidth(220)

        self.lbl_meta = QLabel()
        self.lbl_meta.setProperty("class", "muted")

        self.progress = QProgressBar()
        self.progress.setRange(0, 100)
        self.progress.setFixedWidth(120)
        self.progress.setTextVisible(True)

        lay = QHBoxLayout(self)
        lay.setContentsMargins(8, 6, 8, 6)
        lay.setSpacing(10)
        lay.addWidget(self.lbl_badge)
        lay.addWidget(self.lbl_title, 1)
        lay.addWidget(self.lbl_meta)
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

        self.update_from_job(job)

    @property
    def job_id(self) -> int:
        return self._job_id

    def update_from_job(self, job: Job) -> None:
        self._job_id = job.job_id
        state = job.state
        badge = _STATE_BADGE.get(state, "")
        color = _STATE_COLOR.get(state, "#b5bcc9")
        total_items = len(job.items)
        done = sum(1 for it in job.items if it.state.is_terminal())
        running = sum(1 for it in job.items if it.state == JobItemState.RUNNING)

        self.lbl_badge.setText(badge)
        self.lbl_title.setText(job.label or f"Job {job.job_id}")
        self.lbl_title.setToolTip(self.lbl_title.text())

        if total_items:
            status = f"{done}/{total_items} items"
        else:
            status = "0 items"
        if running:
            status += " - running"
        self.lbl_meta.setText(status)
        self.lbl_meta.setStyleSheet(f"color: {color};")

        self.progress.setMaximum(100)
        self.progress.setValue(job.progress_percent())
