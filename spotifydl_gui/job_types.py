"""
Job and queue data structures for the revamped spotify-dl GUI scheduler.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field, asdict
from enum import Enum
from typing import Dict, List, Optional


class QueueSource(str, Enum):
    """Origin of a job submission."""

    MANUAL = "manual"
    WEB = "web"
    SENTRY = "sentry"


class JobState(str, Enum):
    """Lifecycle state of a job."""

    PENDING = "pending"
    RUNNING = "running"
    PAUSED = "paused"
    SUCCESS = "success"
    FAILED = "failed"
    CANCELLED = "cancelled"

    def is_terminal(self) -> bool:
        return self in {JobState.SUCCESS, JobState.FAILED, JobState.CANCELLED}


class JobItemState(str, Enum):
    """Lifecycle state for individual URLs inside a job."""

    PENDING = "pending"
    RUNNING = "running"
    SUCCESS = "success"
    FAILED = "failed"
    SKIPPED = "skipped"
    CANCELLED = "cancelled"

    def is_terminal(self) -> bool:
        return self in {
            JobItemState.SUCCESS,
            JobItemState.FAILED,
            JobItemState.SKIPPED,
            JobItemState.CANCELLED,
        }


@dataclass
class JobItem:
    """Single Spotify URL entry that belongs to a Job."""

    item_id: int
    url: str
    state: JobItemState = JobItemState.PENDING
    progress: int = 0
    log_excerpt: str = ""
    error: str = ""
    meta: Dict[str, str] = field(default_factory=dict)

    def set_progress(self, pct: int) -> None:
        pct = max(0, min(100, int(pct)))
        self.progress = pct

    def to_dict(self) -> Dict:
        data = asdict(self)
        data["state"] = self.state.value
        return data

    @classmethod
    def from_dict(cls, payload: Dict) -> "JobItem":
        state = payload.get("state", JobItemState.PENDING.value)
        try:
            state_enum = JobItemState(state)
        except ValueError:
            state_enum = JobItemState.PENDING
        meta = payload.get("meta") or {}
        if not isinstance(meta, dict):
            meta = {}
        return cls(
            item_id=int(payload.get("item_id", 0)),
            url=str(payload.get("url", "")),
            state=state_enum,
            progress=int(payload.get("progress", 0)),
            log_excerpt=str(payload.get("log_excerpt", "")),
            error=str(payload.get("error", "")),
            meta=meta,
        )


@dataclass
class Job:
    """Collection of URLs that will be processed sequentially by the runner."""

    job_id: int
    label: str
    source: QueueSource = QueueSource.MANUAL
    items: List[JobItem] = field(default_factory=list)
    created_at: float = field(default_factory=time.time)
    started_at: Optional[float] = None
    finished_at: Optional[float] = None
    state: JobState = JobState.PENDING
    options: Dict[str, object] = field(default_factory=dict)
    last_error: str = ""
    auto_remove: bool = False

    def add_item(self, item: JobItem) -> None:
        self.items.append(item)

    def remove_item(self, item_id: int) -> None:
        self.items = [it for it in self.items if it.item_id != item_id]

    def pending_items(self) -> List[JobItem]:
        return [it for it in self.items if it.state == JobItemState.PENDING]

    def active_item(self) -> Optional[JobItem]:
        for it in self.items:
            if it.state == JobItemState.RUNNING:
                return it
        return None

    def first_pending(self) -> Optional[JobItem]:
        for it in self.items:
            if it.state == JobItemState.PENDING:
                return it
        return None

    def progress_percent(self) -> int:
        if not self.items:
            return 0
        total = sum(min(max(it.progress, 0), 100) for it in self.items)
        return int(total / len(self.items))

    def mark_started(self) -> None:
        if not self.started_at:
            self.started_at = time.time()
        self.state = JobState.RUNNING

    def mark_finished(self, state: JobState) -> None:
        self.state = state
        self.finished_at = time.time()

    def reset(self) -> None:
        self.state = JobState.PENDING
        self.started_at = None
        self.finished_at = None
        self.last_error = ""
        for it in self.items:
            it.state = JobItemState.PENDING
            it.progress = 0
            it.error = ""
            it.log_excerpt = ""

    def to_dict(self) -> Dict:
        return {
            "job_id": self.job_id,
            "label": self.label,
            "source": self.source.value,
            "items": [it.to_dict() for it in self.items],
            "created_at": self.created_at,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "state": self.state.value,
            "options": self.options,
            "last_error": self.last_error,
            "auto_remove": self.auto_remove,
        }

    @classmethod
    def from_dict(cls, payload: Dict) -> "Job":
        try:
            source = QueueSource(payload.get("source", QueueSource.MANUAL.value))
        except ValueError:
            source = QueueSource.MANUAL
        try:
            state = JobState(payload.get("state", JobState.PENDING.value))
        except ValueError:
            state = JobState.PENDING

        items_payload = payload.get("items") or []
        items: List[JobItem] = []
        for item_payload in items_payload:
            try:
                items.append(JobItem.from_dict(item_payload))
            except Exception:
                continue

        opts = payload.get("options") or {}
        if not isinstance(opts, dict):
            opts = {}

        return cls(
            job_id=int(payload.get("job_id", 0)),
            label=str(payload.get("label", "Job")),
            source=source,
            items=items,
            created_at=float(payload.get("created_at" or 0)),
            started_at=payload.get("started_at"),
            finished_at=payload.get("finished_at"),
            state=state,
            options=opts,
            last_error=str(payload.get("last_error", "")),
            auto_remove=bool(payload.get("auto_remove", False)),
        )

    def clone_shallow(self) -> "Job":
        """Create a shallow copy without duplicating item identity."""
        return Job(
            job_id=self.job_id,
            label=self.label,
            source=self.source,
            items=list(self.items),
            created_at=self.created_at,
            started_at=self.started_at,
            finished_at=self.finished_at,
            state=self.state,
            options=dict(self.options),
            last_error=self.last_error,
            auto_remove=self.auto_remove,
        )
