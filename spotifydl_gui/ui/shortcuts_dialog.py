# spotifydl_gui/ui/shortcuts_dialog.py
"""
Simple dialog listing keyboard shortcuts and their functions.
"""

from __future__ import annotations

from typing import List, Tuple

from PySide6.QtCore import Qt
from PySide6.QtWidgets import QDialog, QVBoxLayout, QLabel, QListWidget, QListWidgetItem, QDialogButtonBox


class ShortcutsDialog(QDialog):
    def __init__(self, parent, entries: List[Tuple[str, str]]):
        super().__init__(parent)
        self.setWindowTitle("Keyboard Shortcuts")
        self.setMinimumWidth(520)

        layout = QVBoxLayout(self)

        intro = QLabel("Handy shortcuts throughout the app:")
        layout.addWidget(intro)

        self.list = QListWidget()
        for keys, desc in entries:
            item = QListWidgetItem(f"{keys} — {desc}")
            self.list.addItem(item)
        layout.addWidget(self.list)

        hint = QLabel("Tip: Press F1 to open this list at any time.")
        hint.setProperty("class", "muted")
        layout.addWidget(hint)

        btns = QDialogButtonBox(QDialogButtonBox.Close)
        btns.rejected.connect(self.reject)
        btns.accepted.connect(self.accept)
        btns.button(QDialogButtonBox.Close).clicked.connect(self.close)
        layout.addWidget(btns)

