# spotifydl_gui/main.py
"""
spotify-dl GUI — v0.6 (modular)
Entry point: creates the Qt app, applies theme, and shows the main window.
"""

from __future__ import annotations

import sys
import platform
import ctypes
from PySide6.QtWidgets import QApplication
from PySide6.QtGui import QIcon

# Local modules
from .theme import apply_dark_theme
from .utils import get_app_icon
from .ui.main_window import MainWindow
from .settings_store import APP_NAME, APP_ORG


def _set_windows_app_id():
    """Ensure Windows notifications/taskbar group under our identity."""
    if platform.system() == "Windows":
        try:
            ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID("JoshTools.spotifydl.gui")
        except Exception:
            # Non-fatal: keep going without AUMID
            pass


def main():
    _set_windows_app_id()

    app = QApplication(sys.argv)
    app.setApplicationName(APP_NAME)
    app.setOrganizationName(APP_ORG)

    icon: QIcon = get_app_icon()
    app.setWindowIcon(icon)

    apply_dark_theme(app)

    win = MainWindow(app_icon=icon)  # pass down for tray reuse
    win.show()

    sys.exit(app.exec())


if __name__ == "__main__":
    main()
