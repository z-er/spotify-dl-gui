# spotifydl_gui/runner.py
"""
Queue runner for spotify-dl GUI (v0.7).

Responsibilities
- Spawn spotify-dl via QProcess per-URL, stream merged stdout/stderr
- Live-tail output to UI (chunked), parse % to drive per-row progress
- Backoff between jobs + detect "rate limit" patterns
- Adaptive parallelism (temporary adjust --parallel up/down)
- Organize new files after each job (folder template, dup handling, covers, integrity)
- Smart Sync (optional): for playlist URLs, mirror playlist folder contents to outputs
- Write rich per-job logs (+ optional M3U8)
- Aggregate stats and notify UI via Qt signals

The UI owns:
- Persistent terminal (we emit the command line)
- Scheduler (UI triggers Runner.start with a prepared queue)
- Settings dialogs, history UI
"""

from __future__ import annotations

import os
import sys
import re
import shlex
import json
import time
from dataclasses import dataclass, asdict
from datetime import datetime
from pathlib import Path
from typing import List, Dict, Optional, Tuple, Set

from PySide6.QtCore import QObject, QProcess, QTimer, QIODevice, Signal

from .utils import resolve_spotifydl_binary
from .organizer import (
    organize_new_files,
    list_audio_files,
)
from .settings_store import APP_VER

# ----------- Regex / constants -----------
SPOTIFY_URL_RE = re.compile(
    r"^(https?://open\.spotify\.com/(track|album|playlist)/[A-Za-z0-9]+(\?.*)?$|spotify:(track|album|playlist):[A-Za-z0-9]+)$",
    re.IGNORECASE,
)
IS_PLAYLIST_RE = re.compile(r"(open\.spotify\.com/playlist/|^spotify:playlist:)", re.IGNORECASE)

PERCENT_RE = re.compile(r"(?<!\d)(\d{1,3})%(?!\d)")
RATE_LIMIT_TOKENS = ("429", "rate limit", "too many requests", "slow down")

THROTTLE_TRACKS_THRESHOLD = 30
BACKOFF_SEQUENCE_SECONDS = [10, 20, 30]
BACKOFF_RESET_ON_SUCCESS = True

# ----------- Options / Results dataclasses -----------
@dataclass
class RunOptions:
    dest: str
    fmt: str = "flac"
    parallel: int = 5
    force: bool = False
    extra: str = ""              # raw extra flags string
    m3u_export: bool = True
    m3u_in_folder_when_single: bool = True
    smart_sync: bool = True      # only applied for playlist URLs
    adaptive_parallel: bool = True
    bin_override: str = ""       # optional; otherwise resolve
    sentry_enabled: bool = False
    sentry_gap_sec: int = 25

    def to_args(self) -> List[str]:
        args = []
        if self.dest:
            args += ["--destination", self.dest]
        if self.fmt:
            args += ["--format", self.fmt]
        args += ["--parallel", str(self.parallel)]
        if self.force:
            args += ["--force"]
        if self.extra.strip():
            args += shlex.split(self.extra)
        # args += ["--verbose"]  # if your fork supports it
        return args


@dataclass
class JobSummary:
    url: str
    start_iso: str
    code: int
    dest: str
    outputs: List[Dict]
    suspects: List[Dict]
    stats: Dict[str, int]
    log_path: str | None


@dataclass
class QueueTotals:
    moved: int = 0
    replaced: int = 0
    deleted: int = 0
    skipped: int = 0
    suspect: int = 0
    ok: int = 0
    fail: int = 0


# ----------- Runner -----------
class Runner(QObject):
    # UI signals
    sig_job_started = Signal(int, int, str)              # index, total, url
    sig_job_log = Signal(int, str)                       # index, chunk text
    sig_job_progress = Signal(int, int)                  # index, percent (0..100)
    sig_job_finished = Signal(int, object)               # index, JobSummary
    sig_queue_finished = Signal(object)                  # QueueTotals
    sig_backoff_updated = Signal(int)                    # remaining seconds (0 = clear)
    sig_command_line = Signal(int, str)                  # index, command string (for persistent terminal mirroring)
    sig_parallel_changed = Signal(int)                   # current effective parallel (adaptive)

    def __init__(self, settings, parent=None):
        super().__init__(parent)
        self.settings = settings

        # Queue state
        self._urls: List[str] = []
        self._idx: int = 0
        self._paused: bool = False
        self._totals = QueueTotals()

        # Process / logging
        self._proc: Optional[QProcess] = None
        self._tail_timer = QTimer(self)
        self._tail_timer.setInterval(500)
        self._tail_timer.timeout.connect(self._tail_tick)
        self._job_temp_log: Optional[str] = None
        self._tail_pos: int = 0
        self._run_started_at: Optional[float] = None
        self._pre_run_files: Optional[Set[str]] = None
        self._raw_collector: List[str] = []   # used in rate-limit detection

        # Backoff
        self._pending_delay_sec: int = 0
        self._backoff_index: int = 0
        self._consecutive_failures: int = 0
        self._backoff_until: float = 0.0

        # Adaptive parallelism
        self._desired_parallel: int = 5
        self._effective_parallel: int = 5
        self._adaptive_on: bool = True

        # Features
        self._m3u_export: bool = True
        self._m3u_in_folder_when_single: bool = True
        self._smart_sync: bool = True
        self._sentry_enabled: bool = False
        self._sentry_gap_sec: int = 25

        # Paths
        self._dest: str = ""
        self._bin: Optional[str] = None

    # ---------- Public API ----------
    def start(self, urls: List[str], opts: RunOptions):
        if self._proc:
            return  # ignore if already running

        urls = [u for u in urls if SPOTIFY_URL_RE.match(u)]
        if not urls:
            return

        # Init state
        self._urls = urls
        self._idx = 0
        self._paused = False
        self._totals = QueueTotals()
        self._desired_parallel = max(1, opts.parallel)
        self._effective_parallel = self._desired_parallel
        self._adaptive_on = bool(opts.adaptive_parallel)
        self._m3u_export = bool(opts.m3u_export)
        self._m3u_in_folder_when_single = bool(opts.m3u_in_folder_when_single)
        self._smart_sync = bool(opts.smart_sync)
        self._sentry_enabled = bool(opts.sentry_enabled)
        try:
            self._sentry_gap_sec = int(opts.sentry_gap_sec)
        except Exception:
            self._sentry_gap_sec = 25

        self._dest = opts.dest
        try:
            self._bin = opts.bin_override.strip() or resolve_spotifydl_binary(self.settings)
        except Exception as e:
            raise

        # Kick off first job
        self._schedule_next_job(0, opts)

    def stop(self):
        if self._proc and self._proc.state() != QProcess.NotRunning:
            self._proc.terminate()
            self._proc.waitForFinished(1500)
            if self._proc and self._proc.state() != QProcess.NotRunning:
                self._proc.kill()
        self._cleanup_after_job()
        self._urls = []
        self._idx = 0
        self._paused = False
        self._pending_delay_sec = 0
        self.sig_backoff_updated.emit(0)

    def pause_toggle(self):
        self._paused = not self._paused
        if not self._paused and not self._proc:
            # resume immediately if nothing is running
            self._schedule_next_job(0, None)

    def is_running(self) -> bool:
        return bool(self._proc)

    def current_effective_parallel(self) -> int:
        return int(self._effective_parallel)

    # ---------- Internals ----------
    def _build_args(self, opts: RunOptions) -> List[str]:
        # Enforce current effective parallel
        args = []
        if opts.dest:
            args += ["--destination", opts.dest]
        if opts.fmt:
            args += ["--format", opts.fmt]
        args += ["--parallel", str(self._effective_parallel)]
        if opts.force:
            args += ["--force"]
        if opts.extra.strip():
            args += shlex.split(opts.extra)
        return args

    def _schedule_next_job(self, delay_sec: int, opts: Optional[RunOptions]):
        if self._paused:
            return
        self._pending_delay_sec = max(0, int(delay_sec))
        if delay_sec > 0:
            self._backoff_until = time.time() + delay_sec
            self._tick_backoff_banner()
        else:
            self.sig_backoff_updated.emit(0)
        # If a process is running, we'll continue after it ends; otherwise start now/after delay
        if not self._proc:
            if self._pending_delay_sec > 0:
                QTimer.singleShot(self._pending_delay_sec * 1000, lambda: self._start_next_job(opts))
            else:
                self._start_next_job(opts)

    def _tick_backoff_banner(self):
        remain = int(self._backoff_until - time.time())
        self.sig_backoff_updated.emit(max(0, remain))
        if remain > 0:
            QTimer.singleShot(1000, self._tick_backoff_banner)

    def _start_next_job(self, opts: Optional[RunOptions]):
        if self._idx >= len(self._urls):
            # queue finished
            self.sig_queue_finished.emit(self._totals)
            return
        if not opts:
            # rehydrate minimal opts from current state
            opts = RunOptions(
                dest=self._dest,
                fmt="flac",
                parallel=self._desired_parallel,
                force=False,
            )
            opts.m3u_export = self._m3u_export
            opts.m3u_in_folder_when_single = self._m3u_in_folder_when_single
            opts.smart_sync = self._smart_sync
            opts.adaptive_parallel = self._adaptive_on
            opts.bin_override = self._bin or ""
            opts.sentry_enabled = self._sentry_enabled
            opts.sentry_gap_sec = self._sentry_gap_sec


        url = self._urls[self._idx]
        self.sig_job_started.emit(self._idx, len(self._urls), url)

        # Snapshot existing files for organizer
        self._run_started_at = time.time()
        self._pre_run_files = list_audio_files(Path(opts.dest)) if opts.dest else set()

        # Build command
        args = self._build_args(opts) + [url]
        program = self._bin or resolve_spotifydl_binary(self.settings)

        # Prepare job temp log
        logs_dir = Path(opts.dest) / "_logs" if opts.dest else Path.cwd() / "_logs"
        logs_dir.mkdir(parents=True, exist_ok=True)
        tmp_name = f"raw_{datetime.now().strftime('%Y%m%d_%H%M%S')}_{self._idx+1:02d}.tmp"
        self._job_temp_log = str(logs_dir / tmp_name)

        # Start process (merged channels -> file)
        self._proc = QProcess(self)
        self._proc.setProcessChannelMode(QProcess.MergedChannels)
        self._proc.setStandardOutputFile(self._job_temp_log, QIODevice.Append)
        self._proc.finished.connect(lambda code, status: self._on_finished(code, status, opts))
        self._proc.errorOccurred.connect(lambda err: None)  # handled by finished path
        self._proc.start(program, args)

        # Live tail
        self._tail_pos = 0
        self._tail_timer.start()
        self._raw_collector = []

        # Emit the composed command line for external mirroring (persistent terminal)
        pretty_cmd = " ".join([program] + [shlex.quote(a) for a in args])
        self.sig_command_line.emit(self._idx, pretty_cmd)

    def _tail_tick(self):
        if not self._job_temp_log:
            return
        try:
            with open(self._job_temp_log, "rb") as f:
                f.seek(self._tail_pos)
                data = f.read()
                self._tail_pos = f.tell()
        except Exception:
            return
        if not data:
            return
        text = data.decode("utf-8", "ignore")
        # Stream to UI
        self.sig_job_log.emit(self._idx, text.replace("\r", "\n"))
        self._raw_collector.append(text.lower())
        # Parse % progress for row
        for m in PERCENT_RE.finditer(text):
            try:
                pct = int(m.group(1))
                if 0 <= pct <= 100:
                    self.sig_job_progress.emit(self._idx, pct)
            except Exception:
                pass

    def _saw_rate_limit(self) -> bool:
        if not self._raw_collector:
            return False
        txt = "".join(self._raw_collector)
        return any(tok in txt for tok in RATE_LIMIT_TOKENS)

    def _cleanup_after_job(self):
        if self._proc:
            try:
                self._proc.deleteLater()
            except Exception:
                pass
        self._proc = None
        self._tail_timer.stop()
        self._tail_pos = 0
        # remove temp file reading responsibility; actual file cleanup happens after we read content

    def _write_log(self, dest_dir: str, url: str, start_iso: str,
                   outputs: List[Dict], suspects: List[Dict], stats: Dict[str, int],
                   raw_text: str, url_index: int) -> Optional[str]:
        try:
            logs_dir = Path(dest_dir) / "_logs" if dest_dir else Path.cwd() / "_logs"
            logs_dir.mkdir(parents=True, exist_ok=True)
            first = outputs[0] if outputs else {}
            artist_slug = self._slug(first.get("artist", "")) if first.get("artist") else ""
            album_slug  = self._slug(first.get("album", "")) if first.get("album") else ""
            ts = datetime.now().strftime("%Y%m%d_%H%M%S")
            base = f"run_{ts}_{url_index:02d}"
            if artist_slug or album_slug:
                base += "_" + "_".join([p for p in (artist_slug, album_slug) if p])
            log_path = logs_dir / f"{base}.txt"

            header_lines = []
            header_lines.append("=== spotify-dl GUI Job ===")
            header_lines.append(f"Started: {start_iso}")
            header_lines.append(f"Destination: {dest_dir}")
            header_lines.append(f"Input URL: {url}")
            header_lines.append("")

            sus_by_path = {s["dest"]: s for s in (suspects or [])}

            header_lines.append(f"Outputs ({len(outputs)}):")
            for o in outputs:
                size_mb = f"{(o.get('size',0) / (1024*1024)):.2f} MB" if o.get('size',0) > 0 else "?"
                artist = o.get("artist",""); title=o.get("title",""); album=o.get("album",""); dst=o.get("dest","")
                sp = sus_by_path.get(dst)
                suffix = f"  [SUSPECT: {sp['reason']}]" if sp else ""
                header_lines.append(f"  - {artist} – {title} [{album}]{suffix}")
                header_lines.append(f"    → {dst}  ({size_mb})")

            if suspects:
                header_lines.append("")
                header_lines.append(f"Suspect files ({len(suspects)}):")
                for s in suspects:
                    sz = (s["size"] / (1024*1024)) if s["size"] > 0 else 0
                    dstr = f"{s['duration']:.1f}s" if isinstance(s.get("duration"), float) else "?"
                    header_lines.append(f"  - {s.get('artist','')} – {s.get('title','')} [{s.get('album','')}]")
                    header_lines.append(f"    → {s['dest']}  ({sz:.2f} MB, {dstr})  — {s['reason']}")

            header_lines.append("")
            header_lines.append("=== Raw output ===")
            header_lines.append("")

            appendix = {
                "started": start_iso,
                "dest": dest_dir,
                "input": url,
                "outputs": outputs,
                "suspects": suspects,
                "stats": stats,
                "app_ver": APP_VER,
            }

            with open(log_path, "w", encoding="utf-8", errors="ignore") as f:
                f.write("\n".join(header_lines))
                f.write(raw_text.replace("\r", "\n"))
                f.write("\n\n=== Summary (JSON) ===\n")
                f.write(json.dumps(appendix, ensure_ascii=False, indent=2))
            return str(log_path)
        except Exception:
            return None

    def _maybe_write_m3u8(self, dest_dir: str, outputs: List[Dict]) -> None:
        if not self._m3u_export or not outputs:
            return
        # If all files share the same parent dir and setting allows, drop M3U there; otherwise in dest/_playlists.
        parent_dirs = {str(Path(o["dest"]).parent) for o in outputs if o.get("dest")}
        if len(parent_dirs) == 1 and self._m3u_in_folder_when_single:
            target_dir = Path(list(parent_dirs)[0])
        else:
            target_dir = Path(dest_dir) / "_playlists"
            target_dir.mkdir(parents=True, exist_ok=True)

        first = outputs[0]
        artist = first.get("artist", "") or "Various"
        album = first.get("album", "") or "Playlist"
        ts = datetime.now().strftime("%Y%m%d_%H%M%S")
        name_slug = self._slug(f"{artist}_{album}") if len(parent_dirs) == 1 else self._slug("playlist")
        m3u_path = target_dir / f"{name_slug}_{ts}.m3u8"
        try:
            with open(m3u_path, "w", encoding="utf-8", errors="ignore") as f:
                f.write("#EXTM3U\n")
                for o in outputs:
                    p = o.get("dest", "")
                    if not p:
                        continue
                    f.write(f"#EXTINF:-1,{o.get('artist','')} - {o.get('title','')}\n")
                    f.write(p + "\n")
        except Exception:
            pass

    def _smart_sync_prune(self, dest_dir: str, outputs: List[Dict], url: str) -> None:
        """If URL is a playlist and smart-sync is enabled, delete local files in the album/playlist folder
        that are not part of the outputs from this run. Conservative: only acts when all outputs share a parent."""
        if not self._smart_sync or not outputs or not IS_PLAYLIST_RE.search(url):
            return
        parent_dirs = {str(Path(o["dest"]).parent) for o in outputs if o.get("dest")}
        if len(parent_dirs) != 1:
            return
        folder = Path(list(parent_dirs)[0])
        wanted = {Path(o["dest"]).name for o in outputs}
        try:
            for p in folder.iterdir():
                if not p.is_file():
                    continue
                if p.suffix.lower() not in (".flac", ".mp3", ".m4a", ".mp4", ".opus", ".ogg", ".wav"):
                    continue
                if p.name not in wanted:
                    # prune stray track from older sync
                    try:
                        p.unlink()
                    except Exception:
                        pass
        except Exception:
            pass

    def _on_finished(self, code, status, opts: RunOptions):
        # Freeze live tail and fetch raw
        self._tail_timer.stop()
        raw_text = ""
        if self._job_temp_log and Path(self._job_temp_log).exists():
            try:
                raw_text = Path(self._job_temp_log).read_text(encoding="utf-8", errors="ignore")
            except Exception:
                raw_text = ""
            try:
                Path(self._job_temp_log).unlink(missing_ok=True)
            except Exception:
                pass
        self._job_temp_log = None

        # Organizer: compute outputs/suspects/stats
        outputs: List[Dict]; suspects: List[Dict]; stats: Dict[str, int]
        try:
            outputs, suspects, stats = organize_new_files(
                dest_root=opts.dest,
                pre_files=self._pre_run_files,
                run_started_at=self._run_started_at,
                settings=self.settings,
            )
        except Exception:
            outputs, suspects, stats = [], [], {"moved": 0, "replaced": 0, "deleted": 0, "skipped": 0}

        # Smart Sync (playlist only)
        try:
            self._smart_sync_prune(opts.dest, outputs, self._urls[self._idx])
        except Exception:
            pass

        # M3U8
        try:
            self._maybe_write_m3u8(opts.dest, outputs)
        except Exception:
            pass

        # Update totals
        self._totals.moved += stats.get("moved", 0)
        self._totals.replaced += stats.get("replaced", 0)
        self._totals.deleted += stats.get("deleted", 0)
        self._totals.skipped += stats.get("skipped", 0)
        self._totals.suspect += len(suspects)
        if int(code) == 0:
            self._totals.ok += 1
        else:
            self._totals.fail += 1

        # Write log
        start_iso = datetime.fromtimestamp(self._run_started_at or time.time()).strftime("%Y-%m-%d %H:%M:%S")
        log_path = self._write_log(opts.dest, self._urls[self._idx], start_iso, outputs, suspects, stats, raw_text, self._idx + 1)

        # Emit summary
        summary = JobSummary(
            url=self._urls[self._idx],
            start_iso=start_iso,
            code=int(code),
            dest=opts.dest,
            outputs=outputs,
            suspects=suspects,
            stats=stats,
            log_path=log_path,
        )
        self.sig_job_finished.emit(self._idx, summary)

        # Adaptive parallelism
        self._adaptive_adjust(int(code), outputs)

        # Compute delay for next
                # Base delay logic (playlist throttle + failures)
        delay_sec = 0
        if len(outputs) >= THROTTLE_TRACKS_THRESHOLD:
            delay_sec = max(delay_sec, BACKOFF_SEQUENCE_SECONDS[0])
        if int(code) != 0:
            self._consecutive_failures += 1
            if self._backoff_index < len(BACKOFF_SEQUENCE_SECONDS) - 1:
                self._backoff_index += 1
            if self._saw_rate_limit():
                self._backoff_index = len(BACKOFF_SEQUENCE_SECONDS) - 1
            delay_sec = max(delay_sec, BACKOFF_SEQUENCE_SECONDS[self._backoff_index])
        else:
            if BACKOFF_RESET_ON_SUCCESS:
                self._consecutive_failures = 0
                self._backoff_index = 0

        # NEW: Sentry enforces a minimum fixed gap between jobs
        if self._sentry_enabled:
            delay_sec = max(delay_sec, max(5, int(self._sentry_gap_sec)))


        # Cleanup and advance
        self._cleanup_after_job()
        self._pre_run_files = None
        self._run_started_at = None
        self._raw_collector = []
        self._idx += 1

        # Next
        self._schedule_next_job(delay_sec, opts)

    # ---------- Helpers ----------
    @staticmethod
    def _slug(s: str, maxlen: int = 40) -> str:
        s = re.sub(r"[^\w\-]+", "_", s.strip(), flags=re.UNICODE)
        s = re.sub(r"_+", "_", s).strip("_")
        return (s[:maxlen]).rstrip("_") or "log"

    def _adaptive_adjust(self, exit_code: int, outputs: List[Dict]) -> None:
        """Adjust effective parallelism based on failures / successes."""
        if not self._adaptive_on:
            return
        old = self._effective_parallel
        if exit_code != 0:
            # aggressive drop on failure, but not below 1
            self._effective_parallel = max(1, self._effective_parallel - 1)
        else:
            # Gradually restore towards desired if clean
            if self._effective_parallel < self._desired_parallel:
                self._effective_parallel += 1
        if self._effective_parallel != old:
            self.sig_parallel_changed.emit(self._effective_parallel)
