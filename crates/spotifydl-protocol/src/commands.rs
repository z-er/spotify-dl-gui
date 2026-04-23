use serde::{Deserialize, Serialize};

use crate::models::{AppSettings, JobId, JobOptions, JobSourceKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceCommand {
    EnqueueUrls {
        urls: Vec<String>,
        source: Option<JobSourceKind>,
        options: Option<JobOptions>,
    },
    AddUrlsToJob {
        job_id: JobId,
        urls: Vec<String>,
    },
    AddUrls {
        urls: Vec<String>,
    },
    StartQueue,
    PauseQueue,
    ResumeQueue,
    CancelJob {
        job_id: JobId,
    },
    RetryFailedItems {
        job_id: JobId,
    },
    RequeueHistoryJob {
        job_id: JobId,
    },
    RemoveJob {
        job_id: JobId,
        from_history: bool,
    },
    ReorderJob {
        job_id: JobId,
        new_position: u32,
    },
    ClearHistory,
    UpdateSettings {
        settings: AppSettings,
    },
    RequestSnapshot,
    AcknowledgeError,
    Tick,
}
