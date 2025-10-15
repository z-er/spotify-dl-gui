"""
Queue runner for spotify-dl GUI (job-based rewrite).
"""

from __future__ import annotations

import json
import re
import shlex
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Set, Tuple, TYPE_CHECKING

from PySide6.QtCore import QIODevice, QObject, QProcess, QTimer, Signal

from .job_types import Job, JobItem, JobItemState, JobState, QueueSource
from .organizer import list_audio_files, organize_new_files
from .settings_store import APP_VER
from .utils import resolve_spotifydl_binary

if TYPE_CHECKING:
    from .job_queue import JobQueue

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


@dataclass
class RunOptions:
    dest: str
    fmt: str = "flac"
    parallel: int = 5
    force: bool = False
    extra: str = ""
    m3u_export: bool = True
    m3u_in_folder_when_single: bool = True
    smart_sync: bool = True
    adaptive_parallel: bool = True
    bin_override: str = ""
    sentry_enabled: bool = False
    sentry_gap_sec: int = 25
    json_events: bool = True
    failure_delay_ms: int = 2000
    failure_delay_multiplier: float = 2.0
    failure_delay_max_ms: int = 60000

    def to_payload(self) -> Dict[str, object]:
        return {
            "dest": self.dest,
            "fmt": self.fmt,
            "parallel": self.parallel,
            "force": self.force,
            "extra": self.extra,
            "m3u_export": self.m3u_export,
            "m3u_in_folder_when_single": self.m3u_in_folder_when_single,
            "smart_sync": self.smart_sync,
            "adaptive_parallel": self.adaptive_parallel,
            "bin_override": self.bin_override,
            "sentry_enabled": self.sentry_enabled,
            "sentry_gap_sec": self.sentry_gap_sec,
            "json_events": self.json_events,
            "failure_delay_ms": self.failure_delay_ms,
            "failure_delay_multiplier": self.failure_delay_multiplier,
            "failure_delay_max_ms": self.failure_delay_max_ms,
        }

    @classmethod
    def from_payload(cls, payload: Optional[Dict[str, object]]) -> "RunOptions":
        payload = payload or {}
        if isinstance(payload, cls):
            return payload

        def _get(key: str, default):
            return payload.get(key, default)

        try:
            parallel = int(_get("parallel", cls.parallel))
        except Exception:
            parallel = cls.parallel
        try:
            failure_delay_ms = int(_get("failure_delay_ms", cls.failure_delay_ms))
        except Exception:
            failure_delay_ms = cls.failure_delay_ms
        try:
            failure_delay_multiplier = float(_get("failure_delay_multiplier", cls.failure_delay_multiplier))
        except Exception:
            failure_delay_multiplier = cls.failure_delay_multiplier
        try:
            failure_delay_max_ms = int(_get("failure_delay_max_ms", cls.failure_delay_max_ms))
        except Exception:
            failure_delay_max_ms = cls.failure_delay_max_ms
        try:
            sentry_gap = int(_get("sentry_gap_sec", cls.sentry_gap_sec))
        except Exception:
            sentry_gap = cls.sentry_gap_sec

        return cls(
            dest=str(_get("dest", "")),
            fmt=str(_get("fmt", "flac")),
            parallel=max(1, parallel),
            force=bool(_get("force", False)),
            extra=str(_get("extra", "")),
            m3u_export=bool(_get("m3u_export", True)),
            m3u_in_folder_when_single=bool(_get("m3u_in_folder_when_single", True)),
            smart_sync=bool(_get("smart_sync", True)),
            adaptive_parallel=bool(_get("adaptive_parallel", True)),
            bin_override=str(_get("bin_override", "")),
            sentry_enabled=bool(_get("sentry_enabled", False)),
            sentry_gap_sec=max(5, sentry_gap),
            json_events=bool(_get("json_events", True)),
            failure_delay_ms=failure_delay_ms,
            failure_delay_multiplier=max(0.1, failure_delay_multiplier),
            failure_delay_max_ms=max(failure_delay_ms, failure_delay_max_ms),
        )


@dataclass
class ItemSummary:
    job_id: int
    item_id: int
    url: str
    start_iso: str
    code: int
    dest: str
    outputs: List[Dict]
    suspects: List[Dict]
    stats: Dict[str, int]
    log_path: Optional[str]


@dataclass
class JobRunTotals:
    moved: int = 0
    replaced: int = 0
    deleted: int = 0
    skipped: int = 0
    suspect: int = 0
    items_ok: int = 0
    items_fail: int = 0
    items_cancelled: int = 0


@dataclass
class JobResult:
    job: Job
    totals: JobRunTotals
    state: JobState
    started_iso: str
    finished_iso: str
    items: List[ItemSummary]


class Runner(QObject):
    """Job-based spotify-dl process controller."""

    sig_job_started = Signal(object)
    sig_job_item_started = Signal(int, int, int, int, str)
    sig_job_item_log = Signal(int, int, str)
    sig_job_item_progress = Signal(int, int, int)
    sig_job_item_finished = Signal(object)
    sig_job_finished = Signal(object)

    # Legacy compatibility (temporary)
    sig_queue_finished = Signal(object)

    sig_backoff_updated = Signal(int)
    sig_command_line = Signal(int, int, str)
    sig_parallel_changed = Signal(int)
    sig_rate_limit_notice = Signal(str)

    def __init__(self, settings, job_queue: Optional["JobQueue"] = None, parent=None):
        super().__init__(parent)
        self.settings = settings
        self.job_queue = job_queue

        self._job: Optional[Job] = None
        self._active_item: Optional[JobItem] = None
        self._current_opts: Optional[RunOptions] = None
        self._item_summaries: List[ItemSummary] = []
        self._job_totals = JobRunTotals()
        self._job_cancelled = False
        self._total_items = 0

        self._proc: Optional[QProcess] = None
        self._tail_timer = QTimer(self)
        self._tail_timer.setInterval(500)
        self._tail_timer.timeout.connect(self._tail_tick)
        self._job_temp_log: Optional[str] = None
        self._tail_pos: int = 0
        self._item_started_at: Optional[float] = None
        self._pre_run_files: Optional[Set[str]] = None
        self._raw_collector: List[str] = []

        self._pending_delay_sec: int = 0
        self._backoff_index: int = 0
        self._consecutive_failures: int = 0
        self._backoff_until: float = 0.0

        self._desired_parallel: int = 5
        self._effective_parallel: int = 5
        self._adaptive_on: bool = True

        self._m3u_export: bool = True
        self._m3u_in_folder_when_single: bool = True
        self._smart_sync: bool = True
        self._sentry_enabled: bool = False
        self._sentry_gap_sec: int = 25
        self._json_events: bool = False
        self._json_buffer: str = ""
        self._failure_delay_ms: int = 2000
        self._failure_delay_multiplier: float = 2.0
        self._failure_delay_max_ms: int = 60000

        self._paused: bool = False
        self._bin: Optional[str] = None
    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------
    def start(self, urls: List[str], opts: RunOptions) -> bool:
        """Legacy entrypoint: run a one-off job without registering it."""
        urls = [u for u in urls if SPOTIFY_URL_RE.match(u)]
        if not urls:
            return False
        job = Job(
            job_id=int(time.time() * 1000) % 1_000_000,
            label="Ad-hoc run",
            source=QueueSource.MANUAL,
            auto_remove=False,
        )
        job.options = opts.to_payload()
        for idx, url in enumerate(urls, start=1):
            job.add_item(JobItem(item_id=idx, url=url))
        return self.start_job(job)

    def start_job(self, job: Job) -> bool:
        if self._proc:
            return False
        pending = [it for it in job.items if it.state == JobItemState.PENDING]
        if not pending:
            return False

        self._job = job
        self._active_item = None
        self._item_summaries = []
        self._job_totals = JobRunTotals()
        self._job_cancelled = False
        self._total_items = len(job.items)
        self._paused = False
        self._json_buffer = ""
        self._raw_collector = []
        self._consecutive_failures = 0
        self._backoff_index = 0
        self._pending_delay_sec = 0

        self._current_opts = self._resolve_run_options(job)
        self._apply_run_options(self._current_opts)

        if self._sentry_enabled:
            self._failure_delay_ms = max(self._failure_delay_ms, 25000)
            if self._failure_delay_max_ms < self._failure_delay_ms:
                self._failure_delay_max_ms = self._failure_delay_ms

        self._update_job_state(job, JobState.RUNNING)
        self.sig_job_started.emit(job)
        self._schedule_next_item(0)
        return True

    def cancel_active_job(self) -> None:
        if not self._job:
            return
        self._job_cancelled = True
        self._paused = False
        if self._proc and self._proc.state() != QProcess.NotRunning:
            self._proc.terminate()
            self._proc.waitForFinished(1500)
            if self._proc and self._proc.state() != QProcess.NotRunning:
                self._proc.kill()
            return
        self._cancel_pending_items()
        self._cleanup_after_item()
        self._finalize_job()

    def stop(self) -> None:
        self.cancel_active_job()

    def pause_job(self) -> None:
        self._paused = True

    def resume_job(self) -> None:
        if not self._paused:
            return
        self._paused = False
        if not self._proc and self._job:
            if self._pending_delay_sec > 0:
                QTimer.singleShot(self._pending_delay_sec * 1000, self._start_next_item)
            else:
                self._start_next_item()

    def pause_toggle(self) -> None:
        if self._paused:
            self.resume_job()
        else:
            self.pause_job()

    def is_running(self) -> bool:
        return self._proc is not None or (self._job is not None and not self._paused)

    def current_effective_parallel(self) -> int:
        return int(self._effective_parallel)
    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------
    def _resolve_run_options(self, job: Job) -> RunOptions:
        opts_payload = job.options if isinstance(job.options, dict) else {}
        opts = RunOptions.from_payload(opts_payload)
        if not opts.dest:
            raise RuntimeError("Destination folder missing for job")
        if opts.failure_delay_multiplier <= 0:
            opts.failure_delay_multiplier = 1.0
        if opts.failure_delay_max_ms < opts.failure_delay_ms:
            opts.failure_delay_max_ms = opts.failure_delay_ms
        try:
            self._bin = opts.bin_override.strip() or resolve_spotifydl_binary(self.settings)
        except Exception as exc:
            raise exc
        return opts

    def _apply_run_options(self, opts: RunOptions) -> None:
        self._desired_parallel = max(1, int(opts.parallel))
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
        self._json_events = bool(opts.json_events)
        self._failure_delay_ms = int(opts.failure_delay_ms)
        self._failure_delay_multiplier = float(opts.failure_delay_multiplier)
        self._failure_delay_max_ms = int(opts.failure_delay_max_ms)

    def _build_args(self, opts: RunOptions) -> List[str]:
        args: List[str] = []
        if opts.dest:
            args += ["--destination", opts.dest]
        if opts.fmt:
            args += ["--format", opts.fmt]
        args += ["--parallel", str(self._effective_parallel)]
        if opts.force:
            args += ["--force"]
        if opts.extra.strip():
            args += shlex.split(opts.extra)
        if opts.json_events:
            args += ["--json-events"]
        args += [
            "--failure-delay-ms", str(int(opts.failure_delay_ms)),
            "--failure-delay-multiplier", str(float(opts.failure_delay_multiplier)),
            "--failure-delay-max-ms", str(int(opts.failure_delay_max_ms)),
        ]
        return args

    def _schedule_next_item(self, delay_sec: int) -> None:
        if not self._job:
            return
        if self._paused:
            return
        self._pending_delay_sec = max(0, int(delay_sec))
        if delay_sec > 0:
            self._backoff_until = time.time() + delay_sec
            self._tick_backoff_banner()
        else:
            self.sig_backoff_updated.emit(0)
        if not self._proc:
            if self._pending_delay_sec > 0:
                QTimer.singleShot(self._pending_delay_sec * 1000, self._start_next_item)
            else:
                self._start_next_item()

    def _tick_backoff_banner(self) -> None:
        remain = int(self._backoff_until - time.time())
        self.sig_backoff_updated.emit(max(0, remain))
        if remain > 0:
            QTimer.singleShot(1000, self._tick_backoff_banner)

    def _start_next_item(self) -> None:
        if not self._job or not self._current_opts:
            return
        if self._paused:
            return
        if self._job_cancelled:
            self._cancel_pending_items()
            self._finalize_job()
            return

        next_item = None
        for it in self._job.items:
            if it.state == JobItemState.PENDING:
                next_item = it
                break
        if not next_item:
            self._finalize_job()
            return

        self._active_item = next_item
        self._update_item_state(self._job, next_item, JobItemState.RUNNING, progress=0)

        idx = self._job.items.index(next_item)
        self.sig_job_item_started.emit(
            self._job.job_id,
            next_item.item_id,
            idx + 1,
            self._total_items,
            next_item.url,
        )

        self._item_started_at = time.time()
        self._pre_run_files = set(list_audio_files(Path(self._current_opts.dest))) if self._current_opts.dest else set()

        args = self._build_args(self._current_opts) + [next_item.url]
        program = self._bin or resolve_spotifydl_binary(self.settings)
        logs_dir = Path(self._current_opts.dest) / "_logs" if self._current_opts.dest else Path.cwd() / "_logs"
        logs_dir.mkdir(parents=True, exist_ok=True)
        tmp_name = f"raw_{datetime.now().strftime('%Y%m%d_%H%M%S')}_job{self._job.job_id}_item{idx + 1:02d}.tmp"
        self._job_temp_log = str(logs_dir / tmp_name)

        self._proc = QProcess(self)
        self._proc.setProcessChannelMode(QProcess.MergedChannels)
        self._proc.setStandardOutputFile(self._job_temp_log, QIODevice.Append)
        self._proc.finished.connect(lambda code, status: self._handle_item_finished(code, status))
        self._proc.start(program, args)

        self._tail_pos = 0
        self._tail_timer.start()
        self._raw_collector = []

        pretty_cmd = " ".join([program] + [shlex.quote(a) for a in args])
        self.sig_command_line.emit(self._job.job_id, next_item.item_id, pretty_cmd)

    def _tail_tick(self) -> None:
        if not self._job_temp_log or not self._job or not self._active_item:
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
        cleaned = text.replace("\r", "")
        self._raw_collector.append(cleaned.lower())
        if self._json_events:
            cleaned = self._process_json_events(cleaned)
        if cleaned:
            self.sig_job_item_log.emit(self._job.job_id, self._active_item.item_id, cleaned)
            for m in PERCENT_RE.finditer(cleaned):
                try:
                    pct = int(m.group(1))
                except Exception:
                    continue
                if 0 <= pct <= 100:
                    self.sig_job_item_progress.emit(self._job.job_id, self._active_item.item_id, pct)

    def _process_json_events(self, text: str) -> str:
        buffer = self._json_buffer + text
        ends_with_newline = buffer.endswith("\n")
        parts = buffer.split("\n")
        if ends_with_newline:
            self._json_buffer = ""
        else:
            self._json_buffer = parts.pop() if parts else buffer

        visible: List[str] = []
        for raw_line in parts:
            stripped = raw_line.strip()
            handled = False
            if stripped.startswith("{"):
                try:
                    evt = json.loads(stripped)
                except Exception:
                    evt = None
                if isinstance(evt, dict):
                    self._handle_json_event(evt)
                    handled = True
            if not handled:
                if "[rate-limit" in raw_line.lower():
                    self.sig_rate_limit_notice.emit(raw_line.strip())
                visible.append(raw_line)
        if not visible:
            return ""
        output = "\n".join(visible)
        if ends_with_newline:
            output += "\n"
        return output

    def _handle_json_event(self, evt: dict) -> None:
        if not self._job or not self._active_item:
            return
        event = str(evt.get("event") or evt.get("type") or "").lower()
        if not event:
            return
        if event in {"stage", "stage_update"}:
            progress = evt.get("progress")
            try:
                pct = int(float(progress))
            except Exception:
                pct = None
            if pct is not None and 0 <= pct <= 100:
                self.sig_job_item_progress.emit(self._job.job_id, self._active_item.item_id, pct)
            return
        if event == "track_start":
            self.sig_job_item_progress.emit(self._job.job_id, self._active_item.item_id, 0)
            self.sig_rate_limit_notice.emit("")
            return
        if event == "track_complete":
            self.sig_job_item_progress.emit(self._job.job_id, self._active_item.item_id, 100)
            return
        if event in {"track_failed", "track_skipped"}:
            self.sig_job_item_progress.emit(self._job.job_id, self._active_item.item_id, 0)
            return
        if event == "rate_limit_wait":
            wait_ms = evt.get("wait_ms") or evt.get("delay_ms") or evt.get("wait") or 0
            try:
                wait_ms = int(float(wait_ms))
            except Exception:
                wait_ms = 0
            if wait_ms > 0:
                seconds = max(1, (wait_ms + 999) // 1000)
                self.sig_rate_limit_notice.emit(f"Waiting {seconds}s before next track (rate limit)")
            return
        if event == "rate_limit_backoff":
            delay_ms = evt.get("delay_ms") or evt.get("wait_ms") or evt.get("delay") or 0
            try:
                delay_ms = int(float(delay_ms))
            except Exception:
                delay_ms = 0
            reason = evt.get("reason") or ""
            if delay_ms > 0:
                seconds = max(1, (delay_ms + 999) // 1000)
                msg = f"Cooling down {seconds}s to avoid rate limits"
                if reason:
                    msg += f" ({reason})"
                self.sig_rate_limit_notice.emit(msg)
                self.sig_backoff_updated.emit(seconds)

    def _handle_item_finished(self, code, _status) -> None:
        job = self._job
        item = self._active_item
        opts = self._current_opts
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

        if not (job and item and opts):
            self._cleanup_after_item()
            return

        outputs: List[Dict]
        suspects: List[Dict]
        stats: Dict[str, int]
        try:
            outputs, suspects, stats = organize_new_files(
                dest_root=opts.dest,
                pre_files=self._pre_run_files,
                run_started_at=self._item_started_at,
                settings=self.settings,
            )
        except Exception:
            outputs, suspects, stats = [], [], {"moved": 0, "replaced": 0, "deleted": 0, "skipped": 0}

        try:
            self._smart_sync_prune(opts.dest, outputs, item.url)
        except Exception:
            pass
        try:
            self._maybe_write_m3u8(opts.dest, outputs)
        except Exception:
            pass

        self._job_totals.moved += stats.get("moved", 0)
        self._job_totals.replaced += stats.get("replaced", 0)
        self._job_totals.deleted += stats.get("deleted", 0)
        self._job_totals.skipped += stats.get("skipped", 0)
        self._job_totals.suspect += len(suspects)

        success = int(code) == 0 and not self._job_cancelled
        if success:
            self._job_totals.items_ok += 1
            self._update_item_state(job, item, JobItemState.SUCCESS, progress=100)
        else:
            if self._job_cancelled:
                self._job_totals.items_cancelled += 1
                new_state = JobItemState.CANCELLED
            else:
                self._job_totals.items_fail += 1
                new_state = JobItemState.FAILED
            self._update_item_state(job, item, new_state, progress=0)

        start_iso = datetime.fromtimestamp(self._item_started_at or time.time()).strftime("%Y-%m-%d %H:%M:%S")
        log_path = self._write_log(
            opts.dest,
            job.job_id,
            item.item_id,
            item.url,
            start_iso,
            outputs,
            suspects,
            stats,
            raw_text,
        )
        summary = ItemSummary(
            job_id=job.job_id,
            item_id=item.item_id,
            url=item.url,
            start_iso=start_iso,
            code=int(code),
            dest=opts.dest,
            outputs=outputs,
            suspects=suspects,
            stats=stats,
            log_path=log_path,
        )
        self._item_summaries.append(summary)
        self.sig_job_item_finished.emit(summary)

        self._adaptive_adjust(int(code), outputs)

        delay_sec = 0
        if len(outputs) >= THROTTLE_TRACKS_THRESHOLD:
            delay_sec = max(delay_sec, BACKOFF_SEQUENCE_SECONDS[0])
        if int(code) != 0 and not self._job_cancelled:
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

        if self._sentry_enabled:
            delay_sec = max(delay_sec, max(5, int(self._sentry_gap_sec)))

        self._cleanup_after_item()
        self._schedule_next_item(delay_sec)

    def _cleanup_after_item(self) -> None:
        if self._proc:
            try:
                self._proc.deleteLater()
            except Exception:
                pass
        self._proc = None
        self._tail_timer.stop()
        self._tail_pos = 0
        self._json_buffer = ""
        self._pre_run_files = None
        self._item_started_at = None
        self._raw_collector = []

    def _finalize_job(self) -> None:
        job = self._job
        if not job:
            return
        if any(it.state == JobItemState.PENDING for it in job.items):
            return
        if self._job_cancelled:
            final_state = JobState.CANCELLED
        elif self._job_totals.items_fail > 0:
            final_state = JobState.FAILED
        else:
            final_state = JobState.SUCCESS
        self._update_job_state(job, final_state)

        started_iso = datetime.fromtimestamp(job.started_at or time.time()).strftime("%Y-%m-%d %H:%M:%S")
        finished_iso = datetime.fromtimestamp(job.finished_at or time.time()).strftime("%Y-%m-%d %H:%M:%S")
        job_snapshot = Job.from_dict(job.to_dict()) if hasattr(job, "to_dict") else job
        result = JobResult(
            job=job_snapshot,
            totals=self._job_totals,
            state=final_state,
            started_iso=started_iso,
            finished_iso=finished_iso,
            items=list(self._item_summaries),
        )
        self.sig_job_finished.emit(result)
        self.sig_queue_finished.emit(result)
        if self.job_queue and self.job_queue.active_job() and self.job_queue.active_job().job_id == job.job_id:
            self.job_queue.set_active_job(None)
        self.sig_backoff_updated.emit(0)
        self._job = None
        self._active_item = None
        self._current_opts = None
        self._item_summaries = []
        self._job_totals = JobRunTotals()
        self._job_cancelled = False

    def _cancel_pending_items(self) -> None:
        if not self._job:
            return
        for it in self._job.items:
            if it.state in (JobItemState.PENDING, JobItemState.RUNNING):
                self._update_item_state(self._job, it, JobItemState.CANCELLED)

    def _update_job_state(self, job: Job, state: JobState) -> None:
        if self.job_queue and self.job_queue.get_job(job.job_id):
            self.job_queue.set_job_state(job.job_id, state)
        else:
            if state == JobState.RUNNING:
                job.mark_started()
            elif state.is_terminal():
                job.mark_finished(state)
            else:
                job.state = state

    def _update_item_state(
        self,
        job: Job,
        item: JobItem,
        state: JobItemState,
        progress: Optional[int] = None,
        error: Optional[str] = None,
    ) -> None:
        if self.job_queue and self.job_queue.get_job(job.job_id):
            self.job_queue.set_item_state(job.job_id, item.item_id, state=state, progress=progress, error=error)
        else:
            item.state = state
            if progress is not None:
                item.set_progress(progress)
            if error is not None:
                item.error = error

    def _adaptive_adjust(self, exit_code: int, outputs: List[Dict]) -> None:
        if not self._adaptive_on:
            return
        old = self._effective_parallel
        if exit_code != 0:
            self._effective_parallel = max(1, self._effective_parallel - 1)
        else:
            if self._effective_parallel < self._desired_parallel:
                self._effective_parallel += 1
        if self._effective_parallel != old:
            self.sig_parallel_changed.emit(self._effective_parallel)

    def _saw_rate_limit(self) -> bool:
        if not self._raw_collector:
            return False
        txt = "".join(self._raw_collector)
        return any(tok in txt for tok in RATE_LIMIT_TOKENS)

    def _write_log(
        self,
        dest_dir: str,
        job_id: int,
        item_id: int,
        url: str,
        start_iso: str,
        outputs: List[Dict],
        suspects: List[Dict],
        stats: Dict[str, int],
        raw_text: str,
    ) -> Optional[str]:
        try:
            logs_dir = Path(dest_dir) / "_logs" if dest_dir else Path.cwd() / "_logs"
            logs_dir.mkdir(parents=True, exist_ok=True)
            ts = datetime.now().strftime("%Y%m%d_%H%M%S")
            base = f"run_{ts}_job{job_id}_item{item_id:02d}"
            log_path = logs_dir / f"{base}.txt"

            header_lines = [
                "=== spotify-dl GUI Job Item ===",
                f"Started: {start_iso}",
                f"Destination: {dest_dir}",
                f"Input URL: {url}",
                f"Job ID: {job_id}",
                f"Item ID: {item_id}",
                "",
            ]
            header_lines.append(f"Outputs ({len(outputs)}):")
            for o in outputs:
                artist = o.get("artist", "")
                title = o.get("title", "")
                album = o.get("album", "")
                dst = o.get("dest", "")
                suffix = ""
                size = o.get("size", 0)
                if size:
                    suffix = f" ({size/(1024*1024):.2f} MB)"
                header_lines.append(f"  - {artist} – {title} [{album}] → {dst}{suffix}")
            if suspects:
                header_lines.append("")
                header_lines.append(f"Suspect files ({len(suspects)}):")
                for s in suspects:
                    header_lines.append(
                        f"  - {s.get('artist','')} – {s.get('title','')} [{s.get('album','')}] → {s.get('dest','')}"
                        f" ({s.get('size',0)/(1024*1024):.2f} MB, {s.get('duration','?')}s) — {s.get('reason','')}"
                    )
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
                "job_id": job_id,
                "item_id": item_id,
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
                    try:
                        p.unlink()
                    except Exception:
                        pass
        except Exception:
            pass

    @staticmethod
    def _slug(text: str, maxlen: int = 40) -> str:
        text = re.sub(r"[^\w\-]+", "_", text.strip(), flags=re.UNICODE)
        text = re.sub(r"_+", "_", text).strip("_")
        return (text[:maxlen]).rstrip("_") or "log"
