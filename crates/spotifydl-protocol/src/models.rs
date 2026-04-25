use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type TimestampMs = i64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct JobId(pub String);

impl JobId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Display for JobId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ItemId(pub String);

impl ItemId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Display for ItemId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum QueueStatus {
    #[default]
    Idle,
    Running,
    Paused,
    Backoff,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum JobState {
    #[default]
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ItemState {
    #[default]
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl ItemState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum JobSourceKind {
    #[default]
    Manual,
    Web,
    Sentry,
    Imported,
    Api,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JobSource {
    pub kind: JobSourceKind,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressState {
    pub current: u32,
    pub total: u32,
    pub percent: u8,
    pub detail: String,
}

impl ProgressState {
    pub fn pending(detail: impl Into<String>) -> Self {
        Self {
            current: 0,
            total: 100,
            percent: 0,
            detail: detail.into(),
        }
    }

    pub fn from_steps(current: u32, total: u32, detail: impl Into<String>) -> Self {
        let total = total.max(1);
        let current = current.min(total);
        let percent = ((current as f32 / total as f32) * 100.0).round() as u8;

        Self {
            current,
            total,
            percent,
            detail: detail.into(),
        }
    }

    pub fn finished(detail: impl Into<String>) -> Self {
        Self {
            current: 100,
            total: 100,
            percent: 100,
            detail: detail.into(),
        }
    }
}

impl Default for ProgressState {
    fn default() -> Self {
        Self::pending("Queued")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JobOptions {
    pub destination: String,
    pub format: String,
    pub max_parallel: u16,
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JobTotals {
    pub items_total: u32,
    pub items_completed: u32,
    pub items_failed: u32,
    pub items_cancelled: u32,
    pub outputs_written: u32,
    pub outputs_replaced: u32,
    pub outputs_skipped: u32,
    pub outputs_deleted: u32,
    pub flagged_items: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FailureReasonKind {
    Cancelled,
    Auth,
    RateLimit,
    Network,
    Io,
    Metadata,
    Organizer,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FailureReason {
    pub kind: FailureReasonKind,
    pub code: Option<String>,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ItemMetadata {
    pub artist: String,
    pub album: String,
    pub title: String,
    pub album_artist: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum OutputDisposition {
    #[default]
    Written,
    Replaced,
    KeptBoth,
    SkippedExisting,
    DeletedSmaller,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OutputRecord {
    pub disposition: OutputDisposition,
    pub final_path: String,
    pub artist: String,
    pub album: String,
    pub title: String,
    pub size_bytes: u64,
    pub format: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum IntegritySeverity {
    Info,
    #[default]
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct IntegrityFlag {
    pub severity: IntegritySeverity,
    pub kind: String,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DownloadItem {
    pub id: ItemId,
    pub label: String,
    pub url: String,
    pub state: ItemState,
    pub progress: ProgressState,
    pub created_at_ms: TimestampMs,
    pub updated_at_ms: TimestampMs,
    pub started_at_ms: Option<TimestampMs>,
    pub finished_at_ms: Option<TimestampMs>,
    pub failure_reason: Option<FailureReason>,
    pub metadata: ItemMetadata,
    pub outputs: Vec<OutputRecord>,
    pub flags: Vec<IntegrityFlag>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JobRecord {
    pub id: JobId,
    pub source_url: String,
    pub original_inputs: Vec<String>,
    pub source: JobSource,
    pub label: String,
    pub state: JobState,
    pub progress: ProgressState,
    pub items: Vec<DownloadItem>,
    pub position: u32,
    pub created_at_ms: TimestampMs,
    pub updated_at_ms: TimestampMs,
    pub started_at_ms: Option<TimestampMs>,
    pub finished_at_ms: Option<TimestampMs>,
    pub error_message: Option<String>,
    pub options: JobOptions,
    pub totals: JobTotals,
}

impl JobRecord {
    pub fn new(position: u32, source_url: String, created_at_ms: TimestampMs) -> Self {
        let label = source_url.chars().take(64).collect::<String>();

        Self {
            id: JobId::new(),
            source_url: source_url.clone(),
            original_inputs: vec![source_url.clone()],
            source: JobSource {
                kind: JobSourceKind::Manual,
                summary: source_url.clone(),
            },
            label: if label.is_empty() {
                "Untitled job".to_string()
            } else {
                label
            },
            state: JobState::Queued,
            progress: ProgressState::pending("Queued"),
            items: vec![DownloadItem {
                id: ItemId::new(),
                label: source_url.clone(),
                url: source_url,
                state: ItemState::Queued,
                progress: ProgressState::pending("Waiting for downloader"),
                created_at_ms,
                updated_at_ms: created_at_ms,
                started_at_ms: None,
                finished_at_ms: None,
                failure_reason: None,
                metadata: ItemMetadata::default(),
                outputs: Vec::new(),
                flags: Vec::new(),
            }],
            position,
            created_at_ms,
            updated_at_ms: created_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
            error_message: None,
            options: JobOptions::default(),
            totals: JobTotals {
                items_total: 1,
                ..JobTotals::default()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QueueSnapshot {
    pub status: QueueStatus,
    pub active_job_id: Option<JobId>,
    pub jobs: Vec<JobRecord>,
}

pub type QueueState = QueueSnapshot;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum LogScope {
    #[default]
    Service,
    Backend,
    Storage,
    Gui,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LogEntry {
    pub seq: u64,
    pub timestamp_ms: TimestampMs,
    pub level: LogLevel,
    pub scope: LogScope,
    pub job_id: Option<JobId>,
    pub item_id: Option<ItemId>,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BackoffState {
    pub active: bool,
    pub reason: String,
    pub delay_ms: u64,
    pub remaining_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum BackendKind {
    #[default]
    Fake,
    External,
    Library,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub database_path: String,
    pub default_destination: String,
    pub default_format: String,
    pub auto_add_clipboard_links: bool,
    pub max_parallel: u16,
    pub max_history_entries: usize,
    pub max_log_entries: usize,
    pub theme_mode: ThemeMode,
    pub preferred_backend: BackendKind,
    pub external_backend_executable: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            database_path: String::new(),
            default_destination: String::new(),
            default_format: "flac".to_string(),
            auto_add_clipboard_links: false,
            max_parallel: 5,
            max_history_entries: 100,
            max_log_entries: 250,
            theme_mode: ThemeMode::Dark,
            preferred_backend: BackendKind::Library,
            external_backend_executable: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HistoryEntry {
    pub job: JobRecord,
    pub original_inputs: Vec<String>,
}

impl HistoryEntry {
    pub fn from_job(job: JobRecord) -> Self {
        let original_inputs = if !job.original_inputs.is_empty() {
            job.original_inputs.clone()
        } else if job.items.is_empty() {
            vec![job.source_url.clone()]
        } else {
            job.items
                .iter()
                .map(|item| item.url.clone())
                .collect::<Vec<_>>()
        };

        Self {
            job,
            original_inputs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ServiceHealth {
    pub backend_name: String,
    pub backend_ready: bool,
    pub backend_status_message: String,
    pub database_path: String,
    pub last_recovery_at_ms: Option<TimestampMs>,
    pub last_error: Option<String>,
    pub backoff: BackoffState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSnapshot {
    pub queue: QueueSnapshot,
    pub history: Vec<HistoryEntry>,
    pub logs: Vec<LogEntry>,
    pub settings: AppSettings,
    pub service_health: ServiceHealth,
}

impl Default for AppSnapshot {
    fn default() -> Self {
        Self {
            queue: QueueSnapshot {
                status: QueueStatus::Idle,
                active_job_id: None,
                jobs: Vec::new(),
            },
            history: Vec::new(),
            logs: Vec::new(),
            settings: AppSettings::default(),
            service_health: ServiceHealth::default(),
        }
    }
}
