# spotifydl_gui/updater.py
"""
Simple GitHub-based updater for spotify-dl.exe.

Checks releases, compares versions, and downloads the Windows binary.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
import re
import subprocess
import sys
import threading
from pathlib import Path
from typing import Optional, Tuple
from urllib.request import Request, urlopen

from PySide6.QtCore import QObject, Signal, QStandardPaths

REPO_OWNER = "z-er"
REPO_NAME = "spotify-dl"
ASSET_NAME = "spotify-dl.exe"
LATEST_RELEASE_API = f"https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest"


@dataclass
class UpdateResult:
    status: str
    message: str
    current_version: Optional[str]
    latest_version: Optional[str]
    target_path: Optional[str]
    used_managed: bool = False


def get_base_dir() -> Path:
    return Path(sys.executable).parent if getattr(sys, "frozen", False) else Path(__file__).resolve().parent


def get_managed_dir() -> Path:
    base = QStandardPaths.writableLocation(QStandardPaths.AppDataLocation)
    if not base:
        base = str(Path.home() / ".spotify-dl-gui")
    return Path(base) / "spotify-dl"


def _parse_version(text: Optional[str]) -> Optional[Tuple[int, ...]]:
    if not text:
        return None
    m = re.search(r"(\d+(?:\.\d+)*)", text)
    if not m:
        return None
    parts = m.group(1).split(".")
    try:
        return tuple(int(p) for p in parts)
    except Exception:
        return None


def _is_newer(latest: Optional[str], current: Optional[str]) -> bool:
    lat = _parse_version(latest)
    cur = _parse_version(current)
    if not lat:
        return False
    if not cur:
        return True
    max_len = max(len(lat), len(cur))
    lat_pad = lat + (0,) * (max_len - len(lat))
    cur_pad = cur + (0,) * (max_len - len(cur))
    return lat_pad > cur_pad


def _get_exe_version(exe_path: Path) -> Optional[str]:
    if not exe_path.exists():
        return None
    try:
        out = subprocess.run([str(exe_path), "--version"], capture_output=True, text=True, timeout=3)
        txt = (out.stdout or out.stderr or "").strip()
        return txt.splitlines()[0].strip() if txt else None
    except Exception:
        return None


def _read_release_info() -> dict:
    req = Request(LATEST_RELEASE_API, headers={"User-Agent": "spotify-dl-gui", "Accept": "application/vnd.github+json"})
    with urlopen(req, timeout=15) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _download_asset(url: str, dest: Path) -> None:
    req = Request(url, headers={"User-Agent": "spotify-dl-gui"})
    with urlopen(req, timeout=60) as resp:
        tmp = dest.with_suffix(dest.suffix + ".tmp")
        with open(tmp, "wb") as f:
            while True:
                chunk = resp.read(1024 * 1024)
                if not chunk:
                    break
                f.write(chunk)
        _replace_file(tmp, dest)


def _replace_file(src: Path, dest: Path) -> None:
    backup = dest.with_suffix(dest.suffix + ".old")
    try:
        if dest.exists():
            try:
                dest.replace(backup)
            except Exception:
                pass
        src.replace(dest)
    finally:
        if backup.exists():
            try:
                backup.unlink()
            except Exception:
                pass


def _can_write_dir(path: Path) -> bool:
    try:
        path.mkdir(parents=True, exist_ok=True)
        probe = path / ".write_test"
        with open(probe, "wb") as f:
            f.write(b"")
        probe.unlink()
        return True
    except Exception:
        return False


class SpotifyDlUpdater(QObject):
    sig_finished = Signal(object)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._thread: Optional[threading.Thread] = None

    def check_async(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def _run(self) -> None:
        base_dir = get_base_dir()
        bundled_path = base_dir / ASSET_NAME
        managed_dir = get_managed_dir()
        managed_path = managed_dir / ASSET_NAME

        target_dir = base_dir
        target_path = bundled_path
        used_managed = False

        if not _can_write_dir(base_dir):
            target_dir = managed_dir
            target_path = managed_path
            used_managed = True

        try:
            release = _read_release_info()
        except Exception as exc:
            self.sig_finished.emit(UpdateResult(
                status="error",
                message=f"Failed to reach GitHub: {exc}",
                current_version=_get_exe_version(target_path),
                latest_version=None,
                target_path=str(target_path),
                used_managed=used_managed,
            ))
            return

        if release.get("prerelease") or release.get("draft"):
            self.sig_finished.emit(UpdateResult(
                status="error",
                message="Latest release is marked as prerelease/draft.",
                current_version=_get_exe_version(target_path),
                latest_version=None,
                target_path=str(target_path),
                used_managed=used_managed,
            ))
            return

        tag = release.get("tag_name") or ""
        title = release.get("name") or ""
        latest_version = tag or title or None

        assets = release.get("assets", []) or []
        asset = None
        for a in assets:
            if str(a.get("name", "")).lower() == ASSET_NAME.lower():
                asset = a
                break
        if not asset:
            self.sig_finished.emit(UpdateResult(
                status="error",
                message=f"Release asset not found: {ASSET_NAME}",
                current_version=_get_exe_version(target_path),
                latest_version=latest_version,
                target_path=str(target_path),
                used_managed=used_managed,
            ))
            return

        current_version = _get_exe_version(target_path)
        if not _is_newer(latest_version, current_version):
            self.sig_finished.emit(UpdateResult(
                status="up_to_date",
                message="spotify-dl.exe is already up to date.",
                current_version=current_version,
                latest_version=latest_version,
                target_path=str(target_path),
                used_managed=used_managed,
            ))
            return

        download_url = asset.get("browser_download_url", "")
        if not download_url:
            self.sig_finished.emit(UpdateResult(
                status="error",
                message=f"Release asset URL missing for {ASSET_NAME}",
                current_version=current_version,
                latest_version=latest_version,
                target_path=str(target_path),
                used_managed=used_managed,
            ))
            return

        try:
            target_dir.mkdir(parents=True, exist_ok=True)
            _download_asset(download_url, target_path)
        except Exception as exc:
            self.sig_finished.emit(UpdateResult(
                status="error",
                message=f"Download failed: {exc}",
                current_version=current_version,
                latest_version=latest_version,
                target_path=str(target_path),
                used_managed=used_managed,
            ))
            return

        self.sig_finished.emit(UpdateResult(
            status="updated",
            message="spotify-dl.exe updated.",
            current_version=current_version,
            latest_version=latest_version,
            target_path=str(target_path),
            used_managed=used_managed,
        ))
