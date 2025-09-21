# spotifydl_gui/utils.py
"""
Utility helpers for spotify-dl GUI.

Includes:
- get_app_icon (loads .ico/.png from app dir or system theme)
- binary resolution for spotify-dl
- Windows console helpers for persistent terminal
"""

from __future__ import annotations

import sys
import os
import platform
import ctypes
from pathlib import Path
from shutil import which as _which
from PySide6.QtGui import QIcon


# ----------------------------
# Icons
# ----------------------------
def get_app_icon() -> QIcon:
    """
    Load the application icon.
    Looks for spotify-dl-gui.ico/png next to the frozen exe or source,
    falls back to system theme.
    """
    base = Path(getattr(sys, "_MEIPASS", Path(__file__).parent))
    for name in ("spotify-dl-gui.ico", "spotify-dl-gui.png"):
        p = base / name
        if p.exists():
            return QIcon(str(p))
    return QIcon.fromTheme("audio-x-generic") or QIcon()


# ----------------------------
# Binary resolution
# ----------------------------
def which(cmd: str) -> str | None:
    return _which(cmd)


def resolve_spotifydl_binary(settings) -> str:
    """
    Resolve the spotify-dl executable path according to priority:
    1) Bundled next to app
    2) Custom path from settings
    3) System PATH
    Raises RuntimeError if not found.
    """
    base_dir = Path(sys.executable).parent if getattr(sys, "frozen", False) else Path(__file__).parent
    candidates = ["spotify-dl.exe", "spotify-dl"] if platform.system() == "Windows" else ["spotify-dl"]
    for name in candidates:
        p = base_dir / name
        if p.exists() and p.is_file():
            return str(p)

    custom = (settings.value("bin", "") or "").strip()
    if custom:
        cp = Path(custom)
        if cp.exists() and cp.is_file():
            return str(cp)

    exe = which("spotify-dl") or which("spotify-dl.exe")
    if exe:
        return exe

    raise RuntimeError(
        "spotify-dl executable not found.\n"
        "Place it next to this app, set a custom path in Settings, or add to PATH."
    )


# ----------------------------
# Windows console helpers
# ----------------------------
def console_hwnd_for_pid(pid: int):
    """Find HWND for a console belonging to a given PID (Windows only)."""
    if platform.system() != "Windows":
        return None

    EnumWindows = ctypes.windll.user32.EnumWindows
    GetWindowThreadProcessId = ctypes.windll.user32.GetWindowThreadProcessId
    IsWindowVisible = ctypes.windll.user32.IsWindowVisible
    GetClassNameW = ctypes.windll.user32.GetClassNameW

    hwnds = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def callback(hwnd, lParam):
        if not IsWindowVisible(hwnd):
            return True
        pid_out = ctypes.c_ulong()
        GetWindowThreadProcessId(hwnd, ctypes.byref(pid_out))
        if pid_out.value == pid:
            buf = ctypes.create_unicode_buffer(256)
            GetClassNameW(hwnd, buf, 256)
            if buf.value == "ConsoleWindowClass":
                hwnds.append(hwnd)
        return True

    EnumWindows(callback, 0)
    return hwnds[0] if hwnds else None


def show_window(hwnd, show=True):
    """Show or hide a window by HWND (Windows only)."""
    if platform.system() != "Windows" or not hwnd:
        return
    SW_SHOW, SW_HIDE = 5, 0
    ctypes.windll.user32.ShowWindow(hwnd, SW_SHOW if show else SW_HIDE)
    if show:
        ctypes.windll.user32.SetForegroundWindow(hwnd)
