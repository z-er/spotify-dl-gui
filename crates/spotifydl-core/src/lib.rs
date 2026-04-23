mod external;
mod fake;
mod library;

use spotifydl_protocol::{
    BackoffState, FailureReason, IntegrityFlag, ItemId, ItemMetadata, ItemState, JobId, JobOptions,
    JobState, OutputRecord, ProgressState,
};

pub use external::{
    ExternalCommandSpec, ExternalDownloader, ExternalDownloaderConfig, ExternalDownloaderError,
};
pub use fake::FakeDownloader;
pub use library::{
    LibraryDownloader, LibraryDownloaderConfig, LibraryDownloaderError, ResolvedCollection,
    ResolvedCollectionItem, ResolvedCollectionKind, UpstreamDiscovery, UpstreamIntegrationSurface,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendHealth {
    pub ready: bool,
    pub message: String,
}

impl Default for BackendHealth {
    fn default() -> Self {
        Self {
            ready: true,
            message: "Ready".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BackendCapabilities {
    pub supports_immediate_pause_resume: bool,
}

#[derive(Debug, Clone)]
pub struct BackendItemDescriptor {
    pub item_id: ItemId,
    pub position: u32,
    pub label: String,
    pub url: String,
    pub metadata: ItemMetadata,
}

#[derive(Debug, Clone)]
pub struct BackendRunRequest {
    pub job_id: JobId,
    pub label: String,
    pub source_url: String,
    pub options: JobOptions,
    pub items: Vec<BackendItemDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendControl {
    Pause,
    Resume,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct BackendControlRequest {
    pub job_id: JobId,
    pub control: BackendControl,
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    JobStarted {
        job_id: JobId,
    },
    ItemStarted {
        job_id: JobId,
        item_id: ItemId,
        label: Option<String>,
        metadata: Option<ItemMetadata>,
    },
    ItemProgress {
        job_id: JobId,
        item_id: ItemId,
        progress: ProgressState,
    },
    ItemOutput {
        job_id: JobId,
        item_id: ItemId,
        output: OutputRecord,
    },
    ItemFlag {
        job_id: JobId,
        item_id: ItemId,
        flag: IntegrityFlag,
    },
    ItemFinished {
        job_id: JobId,
        item_id: ItemId,
        state: ItemState,
        failure_reason: Option<FailureReason>,
    },
    JobFinished {
        job_id: JobId,
        state: JobState,
        failure_reason: Option<FailureReason>,
    },
    BackoffStarted {
        job_id: Option<JobId>,
        state: BackoffState,
    },
    BackoffTick {
        job_id: Option<JobId>,
        state: BackoffState,
    },
    Log {
        job_id: Option<JobId>,
        item_id: Option<ItemId>,
        message: String,
    },
}

pub trait DownloadBackend {
    fn name(&self) -> &'static str;
    fn health(&self) -> BackendHealth {
        BackendHealth::default()
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }
    fn validate_run_request(&self, request: &BackendRunRequest) -> Result<(), String> {
        if request.items.is_empty() {
            Err("backend run request did not include any items".to_string())
        } else {
            Ok(())
        }
    }
    fn start(&mut self, request: BackendRunRequest) -> Vec<BackendEvent>;
    fn control(&mut self, request: BackendControlRequest) -> Vec<BackendEvent>;
    fn tick(&mut self) -> Vec<BackendEvent>;
}
