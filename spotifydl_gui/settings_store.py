# spotifydl_gui/settings_store.py
"""
Settings store and constants for spotify-dl GUI.

Centralizes QSettings keys, app metadata, and helper utilities.
"""

from __future__ import annotations
from PySide6.QtCore import QSettings

# Application metadata
APP_NAME = "spotify-dl GUI"
APP_ORG = "JoshTools"
APP_VER = "v0.8"

# Common keys (to avoid typos)
KEYS = {
    "dest": "dest",
    "format": "format",
    "parallel": "parallel",
    "force": "force",
    "extra": "extra",
    "open_when_done": "open_when_done",
    "minimize_to_tray": "minimize_to_tray",
    "persistent_terminal": "persistent_terminal",
    "organize_enabled": "organize_enabled",
    "template": "template",
    "dup_resolve": "dup_resolve",
    "dup_delete_smaller": "dup_delete_smaller",
    "cover_extract": "cover_extract",
    "integrity_flag": "integrity_flag",
    "integrity_min_mb": "integrity_min_mb",
    "integrity_duration_flag": "integrity_duration_flag",
    "integrity_min_seconds": "integrity_min_seconds",
    "m3u_export": "m3u_export",
    "m3u_in_folder_when_single": "m3u_in_folder_when_single",
    "bin": "bin",
    "history": "history",
    "history_max": "history_max",
    "queue_state": "queue_state",
    "job_queue_state": "job_queue_state",
    "scheduler_enabled": "scheduler_enabled",
    "scheduler_time": "scheduler_time",
    "adaptive_parallel": "adaptive_parallel",
    "sentry_enabled": "sentry_enabled",
    "sentry_gap_sec": "sentry_gap_sec",
    "failure_delay_ms": "failure_delay_ms",
    "failure_delay_multiplier": "failure_delay_multiplier",
    "failure_delay_max_ms": "failure_delay_max_ms",
    "web_enabled": "web_enabled",
    "web_host": "web_host",
    "web_port": "web_port",
    "web_username": "web_username",
    "web_password": "web_password",
    "web_dest_override": "web_dest_override",

}


def get_settings() -> QSettings:
    """
    Factory for QSettings, consistently using org/name.
    """
    return QSettings(APP_ORG, APP_NAME)


def read_bool(settings: QSettings, key: str, default: bool = False) -> bool:
    """
    Read a boolean value from QSettings.
    """
    return str(settings.value(key, "true" if default else "false")).lower() == "true"


def write_bool(settings: QSettings, key: str, value: bool) -> None:
    """
    Write a boolean value to QSettings.
    """
    settings.setValue(key, "true" if value else "false")
