"""
JobQueue orchestrates master/job scheduling for the spotify-dl GUI rewrite.
"""

from __future__ import annotations

import json
from typing import Iterable, Iterator, List, Optional, Sequence

from PySide6.QtCore import QObject, Signal

from .job_types import Job, JobItem, JobItemState, JobState, QueueSource
from .settings_store import KEYS


class JobQueue(QObject):
    """In-memory job registry with persistence support."""

    job_added = Signal(object)
    job_removed = Signal(int)
    job_updated = Signal(object)
    queue_reordered = Signal()
    active_job_changed = Signal(object)

    def __init__(self, settings, parent=None):
        super().__init__(parent)
        self._settings = settings
        self._jobs: List[Job] = []
        self._active_job_id: Optional[int] = None
        self._next_job_id: int = 1
        self._next_item_id: int = 1
        self._load_state()

    # ------------- Introspection -------------
    def jobs(self) -> List[Job]:
        return list(self._jobs)

    def iter_jobs(self) -> Iterator[Job]:
        return iter(self._jobs)

    def job_count(self) -> int:
        return len(self._jobs)

    def get_job(self, job_id: int) -> Optional[Job]:
        for job in self._jobs:
            if job.job_id == job_id:
                return job
        return None

    def active_job(self) -> Optional[Job]:
        if self._active_job_id is None:
            return None
        return self.get_job(self._active_job_id)

    def next_pending_job(self) -> Optional[Job]:
        for job in self._jobs:
            if job.state in {JobState.PENDING, JobState.PAUSED} and job.first_pending():
                return job
        return None

    # ------------- Mutation helpers -------------
    def add_job(
        self,
        urls: Sequence[str],
        label: str | None = None,
        source: QueueSource = QueueSource.MANUAL,
        options: Optional[dict] = None,
        auto_remove: bool = False,
    ) -> Job:
        label = (label or "").strip() or f"Job {self._next_job_id}"
        job = Job(
            job_id=self._allocate_job_id(),
            label=label,
            source=source,
            options=dict(options or {}),
            auto_remove=auto_remove,
        )
        for url in urls:
            url = (url or "").strip()
            if not url:
                continue
            item = JobItem(item_id=self._allocate_item_id(), url=url)
            job.add_item(item)
        self._jobs.append(job)
        self.job_added.emit(job)
        self._persist_state()
        return job

    def add_urls_to_job(self, job_id: int, urls: Sequence[str]) -> List[JobItem]:
        job = self.get_job(job_id)
        if not job:
            return []
        created: List[JobItem] = []
        for url in urls:
            url = (url or "").strip()
            if not url:
                continue
            item = JobItem(item_id=self._allocate_item_id(), url=url)
            job.add_item(item)
            created.append(item)
        if created:
            self.job_updated.emit(job)
            self._persist_state()
        return created

    def remove_job(self, job_id: int) -> Optional[Job]:
        job = self.get_job(job_id)
        if not job:
            return None
        self._jobs = [j for j in self._jobs if j.job_id != job_id]
        if self._active_job_id == job_id:
            self._active_job_id = None
            self.active_job_changed.emit(None)
        self.job_removed.emit(job_id)
        self._persist_state()
        return job

    def remove_items(self, job_id: int, item_ids: Iterable[int]) -> List[int]:
        job = self.get_job(job_id)
        if not job:
            return []
        item_ids = {int(i) for i in item_ids}
        removed: List[int] = []
        kept: List[JobItem] = []
        for it in job.items:
            if it.item_id in item_ids:
                removed.append(it.item_id)
            else:
                kept.append(it)
        if removed:
            job.items = kept
            self.job_updated.emit(job)
            self._persist_state()
        return removed

    def move_job(self, job_id: int, new_index: int) -> bool:
        job = self.get_job(job_id)
        if not job:
            return False
        try:
            old_index = self._jobs.index(job)
        except ValueError:
            return False
        if new_index < 0:
            new_index = 0
        if new_index >= len(self._jobs):
            new_index = len(self._jobs) - 1
        if new_index == old_index:
            return False
        self._jobs.pop(old_index)
        self._jobs.insert(new_index, job)
        self.queue_reordered.emit()
        self._persist_state()
        return True

    def clear(self) -> None:
        self._jobs.clear()
        self._active_job_id = None
        self.queue_reordered.emit()
        self.active_job_changed.emit(None)
        self._persist_state()

    # ------------- State mutation -------------
    def set_active_job(self, job_id: Optional[int]) -> None:
        if job_id is not None and not self.get_job(job_id):
            job_id = None
        if self._active_job_id == job_id:
            return
        self._active_job_id = job_id
        job = self.get_job(job_id) if job_id is not None else None
        self.active_job_changed.emit(job)
        self._persist_state()

    def set_job_state(self, job_id: int, state: JobState, error: str | None = None) -> None:
        job = self.get_job(job_id)
        if not job:
            return
        job.state = state
        if state == JobState.RUNNING and not job.started_at:
            job.mark_started()
        if state.is_terminal():
            job.mark_finished(state)
        if error is not None:
            job.last_error = error
        self.job_updated.emit(job)
        self._persist_state()

    def set_item_state(
        self,
        job_id: int,
        item_id: int,
        state: Optional[JobItemState] = None,
        progress: Optional[int] = None,
        error: Optional[str] = None,
        log_excerpt: Optional[str] = None,
    ) -> None:
        job = self.get_job(job_id)
        if not job:
            return
        target = None
        for it in job.items:
            if it.item_id == item_id:
                target = it
                break
        if not target:
            return
        if state is not None:
            target.state = state
        if progress is not None:
            target.set_progress(progress)
        if error is not None:
            target.error = error
        if log_excerpt is not None:
            target.log_excerpt = log_excerpt
        self.job_updated.emit(job)
        self._persist_state()

    # ------------- Persistence -------------
    def _state_payload(self) -> dict:
        return {
            "jobs": [job.to_dict() for job in self._jobs],
            "next_job_id": self._next_job_id,
            "next_item_id": self._next_item_id,
            "active_job_id": self._active_job_id,
        }

    def _persist_state(self) -> None:
        if not self._settings:
            return
        try:
            self._settings.setValue(
                KEYS.get("job_queue_state", "job_queue_state"),
                json.dumps(self._state_payload()),
            )
        except Exception:
            pass

    def _load_state(self) -> None:
        if not self._settings:
            return
        raw = self._settings.value(KEYS.get("job_queue_state", "job_queue_state"), "")
        if not raw:
            return
        try:
            payload = json.loads(raw)
        except Exception:
            return
        jobs_payload = payload.get("jobs") or []
        jobs: List[Job] = []
        max_job_id = 0
        max_item_id = 0
        for job_data in jobs_payload:
            try:
                job = Job.from_dict(job_data)
            except Exception:
                continue
            if not job.items:
                # drop empty jobs that might have been persisted mid-edit
                continue
            jobs.append(job)
            max_job_id = max(max_job_id, job.job_id)
            for it in job.items:
                max_item_id = max(max_item_id, it.item_id)
        self._jobs = jobs
        self._next_job_id = max(max_job_id + 1, int(payload.get("next_job_id", 1)))
        self._next_item_id = max(max_item_id + 1, int(payload.get("next_item_id", 1)))
        self._active_job_id = payload.get("active_job_id")
        if self._active_job_id is not None and not self.get_job(int(self._active_job_id)):
            self._active_job_id = None

    def _allocate_job_id(self) -> int:
        job_id = self._next_job_id
        self._next_job_id += 1
        return job_id

    def _allocate_item_id(self) -> int:
        item_id = self._next_item_id
        self._next_item_id += 1
        return item_id
