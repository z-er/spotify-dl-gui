use serde::{Deserialize, Serialize};

use crate::models::{
    AppSettings, AppSnapshot, BackoffState, DownloadItem, HistoryEntry, ItemId, ItemState, JobId,
    JobRecord, JobState, LogEntry, ProgressState, QueueStatus,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceEvent {
    QueueStatusChanged(QueueStatus),
    JobQueued(JobRecord),
    JobUpdated(JobRecord),
    JobStarted(JobId),
    JobFinished {
        job_id: JobId,
        state: JobState,
    },
    JobRemoved(JobId),
    JobMovedToHistory(JobId),
    ItemStarted {
        job_id: JobId,
        item: DownloadItem,
    },
    ItemProgress {
        job_id: JobId,
        item_id: ItemId,
        progress: ProgressState,
    },
    ItemFinished {
        job_id: JobId,
        item_id: ItemId,
        state: ItemState,
    },
    BackoffStarted(BackoffState),
    BackoffTick(BackoffState),
    LogAppended(LogEntry),
    HistoryUpdated(Vec<HistoryEntry>),
    SettingsUpdated(AppSettings),
    ErrorRaised {
        message: String,
        job_id: Option<JobId>,
        item_id: Option<ItemId>,
    },
    SnapshotUpdated(AppSnapshot),
}
