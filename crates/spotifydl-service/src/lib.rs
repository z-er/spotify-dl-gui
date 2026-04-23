use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use spotifydl_core::{
    BackendControl, BackendControlRequest, BackendEvent, BackendHealth, BackendItemDescriptor,
    BackendRunRequest, DownloadBackend, ExternalDownloader, ExternalDownloaderConfig,
    FakeDownloader,
};
use spotifydl_protocol::{
    AppSettings, AppSnapshot, BackendKind, FailureReason, FailureReasonKind, HistoryEntry, ItemId,
    ItemState, JobId, JobOptions, JobRecord, JobSourceKind, JobState, JobTotals, LogEntry,
    LogLevel, LogScope, ProgressState, QueueStatus, ServiceCommand, ServiceEvent, TimestampMs,
};
use spotifydl_storage::{SqliteStore, StorageError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

pub type ServiceResult<T> = Result<T, ServiceError>;

pub struct SpotifydlService {
    database_path: PathBuf,
    snapshot: AppSnapshot,
    store: SqliteStore,
    backend: Box<dyn DownloadBackend>,
    next_log_seq: u64,
    collection_progress: HashMap<JobId, CollectionProgress>,
}

#[derive(Clone, Debug, Default)]
struct CollectionProgress {
    started_units: u32,
    finished_units: u32,
    current_unit_label: Option<String>,
}

impl SpotifydlService {
    pub fn open(path: impl AsRef<Path>) -> ServiceResult<Self> {
        let database_path = path.as_ref().to_path_buf();
        let store = SqliteStore::open(&database_path)?;
        let snapshot = store.load_snapshot()?.unwrap_or_default();
        let backend = Self::backend_from_settings(&snapshot.settings);

        Self::open_from_parts(database_path, store, snapshot, backend)
    }

    pub fn open_with_backend(
        path: impl AsRef<Path>,
        backend: Box<dyn DownloadBackend>,
    ) -> ServiceResult<Self> {
        let database_path = path.as_ref().to_path_buf();
        let store = SqliteStore::open(&database_path)?;
        let snapshot = store.load_snapshot()?.unwrap_or_default();

        Self::open_from_parts(database_path, store, snapshot, backend)
    }

    fn open_from_parts(
        database_path: PathBuf,
        store: SqliteStore,
        mut snapshot: AppSnapshot,
        backend: Box<dyn DownloadBackend>,
    ) -> ServiceResult<Self> {
        let backend_name = backend.name().to_string();
        snapshot.settings.database_path = database_path.display().to_string();
        snapshot.service_health.database_path = database_path.display().to_string();
        snapshot.service_health.backend_name = backend_name;

        let mut service = Self {
            database_path,
            next_log_seq: snapshot.logs.last().map(|entry| entry.seq + 1).unwrap_or(1),
            snapshot,
            store,
            backend,
            collection_progress: HashMap::new(),
        };

        service.recover_runtime_state();
        service.refresh_backend_health();
        service.persist()?;

        Ok(service)
    }

    fn backend_from_settings(settings: &AppSettings) -> Box<dyn DownloadBackend> {
        match settings.preferred_backend {
            BackendKind::Fake => Box::new(FakeDownloader::default()),
            BackendKind::External => Box::new(ExternalDownloader::new(ExternalDownloaderConfig {
                executable: PathBuf::from(settings.external_backend_executable.clone()),
                working_directory: None,
                base_args: Vec::new(),
                environment: Vec::new(),
            })),
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn snapshot(&self) -> &AppSnapshot {
        &self.snapshot
    }

    pub fn dispatch(&mut self, command: ServiceCommand) -> ServiceResult<Vec<ServiceEvent>> {
        let mut events = Vec::new();

        match command {
            ServiceCommand::EnqueueUrls {
                urls,
                source,
                options,
            } => self.enqueue_urls(urls, source, options, &mut events),
            ServiceCommand::AddUrls { urls } => self.enqueue_urls(urls, None, None, &mut events),
            ServiceCommand::AddUrlsToJob { job_id, urls } => {
                self.add_urls_to_job(&job_id, urls, &mut events)
            }
            ServiceCommand::StartQueue => self.start_queue(&mut events),
            ServiceCommand::PauseQueue => self.pause_queue(&mut events),
            ServiceCommand::ResumeQueue => self.start_queue(&mut events),
            ServiceCommand::CancelJob { job_id } => self.cancel_job(&job_id, &mut events),
            ServiceCommand::RetryFailedItems { job_id } => {
                self.retry_failed_items(&job_id, &mut events)
            }
            ServiceCommand::RequeueHistoryJob { job_id } => {
                self.requeue_history_job(&job_id, &mut events)
            }
            ServiceCommand::RemoveJob {
                job_id,
                from_history,
            } => self.remove_job(&job_id, from_history, &mut events),
            ServiceCommand::ReorderJob {
                job_id,
                new_position,
            } => self.reorder_job(&job_id, new_position, &mut events),
            ServiceCommand::ClearHistory => self.clear_history(&mut events),
            ServiceCommand::UpdateSettings { settings } => {
                let previous_settings = self.snapshot.settings.clone();
                let settings = Self::sanitize_settings(settings);
                self.snapshot.settings = settings.clone();
                self.snapshot.settings.database_path = self.database_path.display().to_string();
                events.push(ServiceEvent::SettingsUpdated(settings));
                self.apply_settings_side_effects(&previous_settings, &mut events);
            }
            ServiceCommand::RequestSnapshot => {}
            ServiceCommand::AcknowledgeError => {}
            ServiceCommand::Tick => self.tick(&mut events),
        }

        self.persist()?;
        events.push(ServiceEvent::SnapshotUpdated(self.snapshot.clone()));

        Ok(events)
    }

    fn enqueue_urls(
        &mut self,
        urls: Vec<String>,
        source: Option<JobSourceKind>,
        options: Option<JobOptions>,
        events: &mut Vec<ServiceEvent>,
    ) {
        let now = now_ms();
        let start_position = self.snapshot.queue.jobs.len() as u32;

        for (offset, url) in urls
            .into_iter()
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .enumerate()
        {
            let mut job = JobRecord::new(start_position + offset as u32, url.clone(), now);
            if let Some(source_kind) = source {
                job.source.kind = source_kind;
            }
            job.options = self.effective_job_options(options.clone());
            self.push_log(
                LogLevel::Info,
                LogScope::Service,
                Some(job.id.clone()),
                None,
                format!("Queued {}", job.label),
                events,
            );
            events.push(ServiceEvent::JobQueued(job.clone()));
            self.snapshot.queue.jobs.push(job);
        }

        self.reindex_queue();
    }

    fn add_urls_to_job(
        &mut self,
        job_id: &JobId,
        urls: Vec<String>,
        events: &mut Vec<ServiceEvent>,
    ) {
        let now = now_ms();
        if let Some(job) = self.find_job_mut(job_id) {
            let start_len = job.items.len() as u32;
            let mut added = 0u32;
            for (offset, url) in urls
                .into_iter()
                .map(|url| url.trim().to_string())
                .filter(|url| !url.is_empty())
                .enumerate()
            {
                let mut temp_job = JobRecord::new(start_len + offset as u32, url, now);
                let item = temp_job.items.remove(0);
                job.items.push(item);
                added += 1;
            }

            if added > 0 {
                job.updated_at_ms = now;
                Self::refresh_job_derived_state(job, None);
                events.push(ServiceEvent::JobUpdated(job.clone()));
            }
        }
    }

    fn start_queue(&mut self, events: &mut Vec<ServiceEvent>) {
        let capabilities = self.backend.capabilities();
        self.set_queue_status(QueueStatus::Running, events);
        self.push_log(
            LogLevel::Info,
            LogScope::Service,
            None,
            None,
            "Queue set to running".to_string(),
            events,
        );

        if let Some(active_job_id) = self.snapshot.queue.active_job_id.clone() {
            let mut should_resume = false;
            if let Some(job) = self.find_job_mut(&active_job_id) {
                if job.state == JobState::Paused {
                    job.state = JobState::Running;
                    job.updated_at_ms = now_ms();
                    for item in &mut job.items {
                        if item.state == ItemState::Paused {
                            item.state = ItemState::Running;
                            item.updated_at_ms = job.updated_at_ms;
                        }
                    }
                    Self::refresh_job_derived_state(job, None);
                    events.push(ServiceEvent::JobUpdated(job.clone()));
                    should_resume = capabilities.supports_immediate_pause_resume;
                }
            }

            if should_resume {
                for backend_event in self.backend.control(BackendControlRequest {
                    job_id: active_job_id,
                    control: BackendControl::Resume,
                }) {
                    self.apply_backend_event(backend_event, events);
                }
            }
        } else {
            self.ensure_active_job(events);
        }
    }

    fn pause_queue(&mut self, events: &mut Vec<ServiceEvent>) {
        let capabilities = self.backend.capabilities();
        self.set_queue_status(QueueStatus::Paused, events);
        self.push_log(
            LogLevel::Info,
            LogScope::Service,
            None,
            None,
            "Queue paused".to_string(),
            events,
        );

        if let Some(active_job_id) = self.snapshot.queue.active_job_id.clone() {
            if capabilities.supports_immediate_pause_resume {
                if let Some(job) = self.find_job_mut(&active_job_id) {
                    job.state = JobState::Paused;
                    job.updated_at_ms = now_ms();
                    job.progress.detail = "Paused".to_string();

                    for item in &mut job.items {
                        if item.state == ItemState::Running {
                            item.state = ItemState::Paused;
                            item.updated_at_ms = job.updated_at_ms;
                            item.progress.detail = "Paused".to_string();
                        }
                    }

                    Self::refresh_job_derived_state(job, None);
                    events.push(ServiceEvent::JobUpdated(job.clone()));
                }

                for backend_event in self.backend.control(BackendControlRequest {
                    job_id: active_job_id,
                    control: BackendControl::Pause,
                }) {
                    self.apply_backend_event(backend_event, events);
                }
                return;
            }

            self.push_log(
                LogLevel::Info,
                LogScope::Service,
                Some(active_job_id),
                None,
                "Backend will pause after the active job reaches a safe boundary".to_string(),
                events,
            );
        }
    }

    fn cancel_job(&mut self, job_id: &JobId, events: &mut Vec<ServiceEvent>) {
        if self.snapshot.queue.active_job_id.as_ref() == Some(job_id) {
            for backend_event in self.backend.control(BackendControlRequest {
                job_id: job_id.clone(),
                control: BackendControl::Cancel,
            }) {
                self.apply_backend_event(backend_event, events);
            }
            return;
        }

        self.finalize_queued_job(
            job_id,
            JobState::Cancelled,
            Some(FailureReason {
                kind: FailureReasonKind::Cancelled,
                code: Some("USER_CANCELLED".to_string()),
                message: "Cancelled by user".to_string(),
                details: None,
            }),
            events,
        );
    }

    fn clear_history(&mut self, events: &mut Vec<ServiceEvent>) {
        self.snapshot.history.clear();
        events.push(ServiceEvent::HistoryUpdated(self.snapshot.history.clone()));
        self.push_log(
            LogLevel::Info,
            LogScope::Service,
            None,
            None,
            "History cleared".to_string(),
            events,
        );
    }

    fn tick(&mut self, events: &mut Vec<ServiceEvent>) {
        let should_tick_backend = self.snapshot.queue.active_job_id.is_some()
            || matches!(
                self.snapshot.queue.status,
                QueueStatus::Running | QueueStatus::Backoff
            );

        if matches!(
            self.snapshot.queue.status,
            QueueStatus::Running | QueueStatus::Backoff
        ) {
            self.ensure_active_job(events);
        }

        if should_tick_backend {
            for backend_event in self.backend.tick() {
                self.apply_backend_event(backend_event, events);
            }

            if self.snapshot.queue.active_job_id.is_none()
                && self.snapshot.queue.status == QueueStatus::Running
            {
                self.ensure_active_job(events);
            }
        }
    }

    fn ensure_active_job(&mut self, events: &mut Vec<ServiceEvent>) {
        if !matches!(
            self.snapshot.queue.status,
            QueueStatus::Running | QueueStatus::Backoff
        ) || self.snapshot.queue.active_job_id.is_some()
        {
            return;
        }

        let Some(next_job_id) = self
            .snapshot
            .queue
            .jobs
            .iter()
            .find(|job| matches!(job.state, JobState::Queued | JobState::Paused))
            .map(|job| job.id.clone())
        else {
            self.set_queue_status(QueueStatus::Idle, events);
            return;
        };

        let Some(job_index) = self.find_job_index(&next_job_id) else {
            return;
        };

        let request = Self::build_backend_request(&self.snapshot.queue.jobs[job_index]);
        let health = self.refresh_backend_health();
        if !health.ready {
            self.report_backend_preflight_failure(Some(next_job_id), health.message, events);
            return;
        }

        if let Err(message) = self.backend.validate_run_request(&request) {
            self.report_backend_preflight_failure(Some(next_job_id), message, events);
            return;
        }

        self.snapshot.service_health.last_error = None;

        {
            let job = &mut self.snapshot.queue.jobs[job_index];
            let now = now_ms();
            job.state = JobState::Running;
            job.updated_at_ms = now;
            job.started_at_ms.get_or_insert(now);
            job.progress.detail = "Waiting for backend events".to_string();
            Self::refresh_job_derived_state(job, None);
            events.push(ServiceEvent::JobUpdated(job.clone()));
        }

        self.snapshot.queue.active_job_id = Some(next_job_id);

        for event in self.backend.start(request) {
            self.apply_backend_event(event, events);
        }
    }

    fn apply_backend_event(&mut self, event: BackendEvent, events: &mut Vec<ServiceEvent>) {
        match event {
            BackendEvent::JobStarted { job_id } => {
                let job_clone = if let Some(job) = self.find_job_mut(&job_id) {
                    job.state = JobState::Running;
                    job.updated_at_ms = now_ms();
                    job.started_at_ms.get_or_insert(job.updated_at_ms);
                    Some(job.clone())
                } else {
                    None
                };

                events.push(ServiceEvent::JobStarted(job_id));
                if let Some(job) = job_clone {
                    events.push(ServiceEvent::JobUpdated(job));
                }
            }
            BackendEvent::ItemStarted {
                job_id,
                item_id,
                label,
                metadata,
            } => {
                let mut item_clone = None;
                let mut job_clone = None;

                if let Some(job_index) = self.find_job_index(&job_id) {
                    let now = now_ms();
                    let collection = {
                        let job = &self.snapshot.queue.jobs[job_index];
                        Self::collection_progress_for_start(
                            &mut self.collection_progress,
                            job,
                            label.as_deref(),
                        )
                    };
                    let job = &mut self.snapshot.queue.jobs[job_index];
                    if let Some(item) = job.items.iter_mut().find(|item| item.id == item_id) {
                        item.state = ItemState::Running;
                        item.updated_at_ms = now;
                        item.started_at_ms.get_or_insert(now);
                        item.progress = ProgressState::pending("Backend started item");
                        if let Some(label) = label {
                            item.label = label;
                        }
                        if let Some(metadata) = metadata {
                            item.metadata = metadata;
                        }
                        item_clone = Some(item.clone());
                    }

                    job.state = JobState::Running;
                    job.updated_at_ms = now;
                    Self::refresh_job_derived_state(job, collection.as_ref());
                    job_clone = Some(job.clone());
                }

                if let Some(item) = item_clone {
                    events.push(ServiceEvent::ItemStarted {
                        job_id: job_id.clone(),
                        item,
                    });
                }
                if let Some(job) = job_clone {
                    events.push(ServiceEvent::JobUpdated(job));
                }
            }
            BackendEvent::ItemProgress {
                job_id,
                item_id,
                progress,
            } => {
                let mut item_progress = None;
                let mut job_clone = None;

                if let Some(job_index) = self.find_job_index(&job_id) {
                    let now = now_ms();
                    let job = &mut self.snapshot.queue.jobs[job_index];
                    if let Some(item) = job.items.iter_mut().find(|item| item.id == item_id) {
                        item.state = ItemState::Running;
                        item.updated_at_ms = now;
                        item.progress = progress.clone();
                        item_progress = Some(item.progress.clone());
                    }

                    job.state = JobState::Running;
                    job.updated_at_ms = now;
                    let collection = self.collection_progress.get(&job.id).cloned();
                    Self::refresh_job_derived_state(job, collection.as_ref());
                    job_clone = Some(job.clone());
                }

                if let Some(progress) = item_progress {
                    events.push(ServiceEvent::ItemProgress {
                        job_id: job_id.clone(),
                        item_id,
                        progress,
                    });
                }
                if let Some(job) = job_clone {
                    events.push(ServiceEvent::JobUpdated(job));
                }
            }
            BackendEvent::ItemOutput {
                job_id,
                item_id,
                output,
            } => {
                if let Some(job_index) = self.find_job_index(&job_id) {
                    let now = now_ms();
                    let job = &mut self.snapshot.queue.jobs[job_index];
                    if let Some(item) = job.items.iter_mut().find(|item| item.id == item_id) {
                        item.outputs.push(output);
                        item.updated_at_ms = now;
                    }

                    job.updated_at_ms = now;
                    let collection = self.collection_progress.get(&job.id).cloned();
                    Self::refresh_job_derived_state(job, collection.as_ref());
                    events.push(ServiceEvent::JobUpdated(job.clone()));
                }
            }
            BackendEvent::ItemFlag {
                job_id,
                item_id,
                flag,
            } => {
                if let Some(job_index) = self.find_job_index(&job_id) {
                    let now = now_ms();
                    let job = &mut self.snapshot.queue.jobs[job_index];
                    if let Some(item) = job.items.iter_mut().find(|item| item.id == item_id) {
                        item.flags.push(flag);
                        item.updated_at_ms = now;
                    }

                    job.updated_at_ms = now;
                    let collection = self.collection_progress.get(&job.id).cloned();
                    Self::refresh_job_derived_state(job, collection.as_ref());
                    events.push(ServiceEvent::JobUpdated(job.clone()));
                }
            }
            BackendEvent::ItemFinished {
                job_id,
                item_id,
                state,
                failure_reason,
            } => {
                let mut job_clone = None;
                let mut error_message = None;

                if let Some(job_index) = self.find_job_index(&job_id) {
                    let now = now_ms();
                    let collection = {
                        let job = &self.snapshot.queue.jobs[job_index];
                        Self::collection_progress_for_finish(&mut self.collection_progress, job)
                    };
                    let job = &mut self.snapshot.queue.jobs[job_index];
                    if let Some(item) = job.items.iter_mut().find(|item| item.id == item_id) {
                        item.state = state;
                        item.updated_at_ms = now;
                        item.finished_at_ms.get_or_insert(now);
                        item.failure_reason = failure_reason.clone();
                        item.progress =
                            ProgressState::finished(Self::terminal_detail_for_item(state));
                    }

                    if matches!(state, ItemState::Failed | ItemState::Cancelled) {
                        error_message = failure_reason
                            .as_ref()
                            .map(|reason| reason.message.clone())
                            .or_else(|| Some(Self::terminal_detail_for_item(state).to_string()));
                    }

                    job.updated_at_ms = now;
                    Self::refresh_job_derived_state(job, collection.as_ref());
                    job_clone = Some(job.clone());
                }

                events.push(ServiceEvent::ItemFinished {
                    job_id: job_id.clone(),
                    item_id: item_id.clone(),
                    state,
                });

                if let Some(message) = error_message {
                    events.push(ServiceEvent::ErrorRaised {
                        message,
                        job_id: Some(job_id.clone()),
                        item_id: Some(item_id),
                    });
                }

                if let Some(job) = job_clone {
                    events.push(ServiceEvent::JobUpdated(job));
                }
            }
            BackendEvent::JobFinished {
                job_id,
                state,
                failure_reason,
            } => {
                self.finish_backend_job(&job_id, state, failure_reason, events);
            }
            BackendEvent::BackoffStarted { job_id, state } => {
                self.snapshot.service_health.backoff = state.clone();
                self.set_queue_status(QueueStatus::Backoff, events);
                events.push(ServiceEvent::BackoffStarted(state));
                self.push_log(
                    LogLevel::Warn,
                    LogScope::Backend,
                    job_id,
                    None,
                    "Backend entered backoff".to_string(),
                    events,
                );
            }
            BackendEvent::BackoffTick { job_id, state } => {
                let became_inactive = !state.active;
                self.snapshot.service_health.backoff = state.clone();
                if became_inactive {
                    let next_status = if self.snapshot.queue.active_job_id.is_some() {
                        QueueStatus::Running
                    } else if self.snapshot.queue.jobs.is_empty() {
                        QueueStatus::Idle
                    } else {
                        QueueStatus::Paused
                    };
                    self.set_queue_status(next_status, events);
                }
                events.push(ServiceEvent::BackoffTick(state));
                if became_inactive {
                    self.push_log(
                        LogLevel::Info,
                        LogScope::Backend,
                        job_id,
                        None,
                        "Backend backoff cleared".to_string(),
                        events,
                    );
                }
            }
            BackendEvent::Log {
                job_id,
                item_id,
                message,
            } => {
                self.push_log(
                    LogLevel::Info,
                    LogScope::Backend,
                    job_id,
                    item_id,
                    message,
                    events,
                );
            }
        }
    }

    fn finish_backend_job(
        &mut self,
        job_id: &JobId,
        state: JobState,
        failure_reason: Option<FailureReason>,
        events: &mut Vec<ServiceEvent>,
    ) {
        let Some(index) = self
            .snapshot
            .queue
            .jobs
            .iter()
            .position(|job| &job.id == job_id)
        else {
            return;
        };

        let mut job = self.snapshot.queue.jobs.remove(index);
        let now = now_ms();
        let mut implied_item_events = Vec::new();

        for item in &mut job.items {
            if item.state.is_terminal() {
                continue;
            }

            item.state = match state {
                JobState::Cancelled => ItemState::Cancelled,
                JobState::Failed => ItemState::Failed,
                _ => ItemState::Completed,
            };
            item.updated_at_ms = now;
            item.finished_at_ms.get_or_insert(now);
            item.failure_reason = if matches!(item.state, ItemState::Failed | ItemState::Cancelled)
            {
                failure_reason.clone()
            } else {
                None
            };
            item.progress = ProgressState::finished(Self::terminal_detail_for_item(item.state));
            implied_item_events.push(ServiceEvent::ItemFinished {
                job_id: job.id.clone(),
                item_id: item.id.clone(),
                state: item.state,
            });
        }

        job.state = state;
        job.updated_at_ms = now;
        job.finished_at_ms = Some(now);
        job.error_message = failure_reason.as_ref().map(|reason| reason.message.clone());
        Self::refresh_job_derived_state(&mut job, None);
        job.progress = ProgressState::finished(Self::terminal_detail_for_job(state));
        self.snapshot.queue.active_job_id = None;
        self.collection_progress.remove(job_id);

        if self.snapshot.service_health.backoff.active {
            self.snapshot.service_health.backoff.active = false;
            self.snapshot.service_health.backoff.remaining_ms = 0;
        }

        for event in implied_item_events {
            events.push(event);
        }

        self.push_log(
            match state {
                JobState::Completed => LogLevel::Info,
                JobState::Cancelled => LogLevel::Warn,
                JobState::Failed => LogLevel::Error,
                _ => LogLevel::Info,
            },
            LogScope::Backend,
            Some(job.id.clone()),
            None,
            format!("Backend finished job as {:?}", state),
            events,
        );
        events.push(ServiceEvent::JobMovedToHistory(job.id.clone()));
        events.push(ServiceEvent::JobFinished {
            job_id: job.id.clone(),
            state: job.state,
        });

        self.snapshot.history.insert(0, HistoryEntry::from_job(job));
        self.trim_history();
        self.reindex_queue();
        events.push(ServiceEvent::HistoryUpdated(self.snapshot.history.clone()));

        if self.snapshot.queue.jobs.is_empty() {
            self.set_queue_status(QueueStatus::Idle, events);
        } else if self.snapshot.queue.status != QueueStatus::Paused {
            self.set_queue_status(QueueStatus::Running, events);
        }
    }

    fn finalize_queued_job(
        &mut self,
        job_id: &JobId,
        state: JobState,
        failure_reason: Option<FailureReason>,
        events: &mut Vec<ServiceEvent>,
    ) {
        let Some(index) = self
            .snapshot
            .queue
            .jobs
            .iter()
            .position(|job| &job.id == job_id)
        else {
            return;
        };

        let mut job = self.snapshot.queue.jobs.remove(index);
        self.collection_progress.remove(job_id);
        let now = now_ms();

        for item in &mut job.items {
            if !item.state.is_terminal() {
                item.state = match state {
                    JobState::Cancelled => ItemState::Cancelled,
                    JobState::Failed => ItemState::Failed,
                    _ => ItemState::Completed,
                };
                item.progress = ProgressState::finished(Self::terminal_detail_for_item(item.state));
                item.failure_reason = failure_reason.clone();
                item.updated_at_ms = now;
                item.finished_at_ms = Some(now);
                events.push(ServiceEvent::ItemFinished {
                    job_id: job.id.clone(),
                    item_id: item.id.clone(),
                    state: item.state,
                });
            }
        }

        job.state = state;
        job.updated_at_ms = now;
        job.finished_at_ms = Some(now);
        job.error_message = failure_reason.as_ref().map(|reason| reason.message.clone());
        Self::refresh_job_derived_state(&mut job, None);
        job.progress = ProgressState::finished(Self::terminal_detail_for_job(state));

        self.push_log(
            LogLevel::Warn,
            LogScope::Service,
            Some(job.id.clone()),
            None,
            "Cancelled job".to_string(),
            events,
        );
        events.push(ServiceEvent::JobFinished {
            job_id: job.id.clone(),
            state: job.state,
        });
        events.push(ServiceEvent::JobMovedToHistory(job.id.clone()));
        self.snapshot.history.insert(0, HistoryEntry::from_job(job));
        self.trim_history();
        self.reindex_queue();
        events.push(ServiceEvent::HistoryUpdated(self.snapshot.history.clone()));

        if self.snapshot.queue.jobs.is_empty() && self.snapshot.queue.active_job_id.is_none() {
            self.set_queue_status(QueueStatus::Idle, events);
        }
    }

    fn find_job_index(&self, job_id: &JobId) -> Option<usize> {
        self.snapshot
            .queue
            .jobs
            .iter()
            .position(|job| &job.id == job_id)
    }

    fn find_job_mut(&mut self, job_id: &JobId) -> Option<&mut JobRecord> {
        self.snapshot
            .queue
            .jobs
            .iter_mut()
            .find(|job| &job.id == job_id)
    }

    fn recover_runtime_state(&mut self) {
        let mut recovered_any = false;

        for job in &mut self.snapshot.queue.jobs {
            if job.state == JobState::Running {
                job.state = JobState::Paused;
                for item in &mut job.items {
                    if item.state == ItemState::Running {
                        item.state = ItemState::Paused;
                        item.progress.detail = "Recovered as paused after restart".to_string();
                    }
                }
                Self::refresh_job_derived_state(job, None);
                recovered_any = true;
            }
        }

        self.snapshot.queue.active_job_id = None;
        self.snapshot.queue.status = if self.snapshot.queue.jobs.is_empty() {
            QueueStatus::Idle
        } else {
            QueueStatus::Paused
        };
        self.snapshot.service_health.backoff.active = false;
        self.snapshot.service_health.backoff.remaining_ms = 0;

        if recovered_any {
            self.snapshot.service_health.last_recovery_at_ms = Some(now_ms());
            let mut ignored_events = Vec::new();
            self.push_log(
                LogLevel::Warn,
                LogScope::Service,
                None,
                None,
                "Recovered persisted runtime state and paused active jobs".to_string(),
                &mut ignored_events,
            );
        }

        self.reindex_queue();
        self.trim_history();
        self.trim_logs();
    }

    fn reindex_queue(&mut self) {
        for (index, job) in self.snapshot.queue.jobs.iter_mut().enumerate() {
            job.position = index as u32;
        }
    }

    fn trim_history(&mut self) {
        let max_history = self.snapshot.settings.max_history_entries.max(1);
        if self.snapshot.history.len() > max_history {
            self.snapshot.history.truncate(max_history);
        }
    }

    fn trim_logs(&mut self) {
        let max_logs = self.snapshot.settings.max_log_entries.max(1);
        if self.snapshot.logs.len() > max_logs {
            let keep_from = self.snapshot.logs.len() - max_logs;
            self.snapshot.logs.drain(0..keep_from);
        }
    }

    fn push_log(
        &mut self,
        level: LogLevel,
        scope: LogScope,
        job_id: Option<JobId>,
        item_id: Option<ItemId>,
        message: String,
        events: &mut Vec<ServiceEvent>,
    ) {
        let entry = LogEntry {
            seq: self.next_log_seq,
            timestamp_ms: now_ms(),
            level,
            scope,
            job_id,
            item_id,
            message,
            details: None,
        };
        self.next_log_seq += 1;

        self.snapshot.logs.push(entry.clone());
        self.trim_logs();
        events.push(ServiceEvent::LogAppended(entry));
    }

    fn persist(&mut self) -> ServiceResult<()> {
        self.store.save_snapshot(&self.snapshot)?;
        Ok(())
    }

    fn retry_failed_items(&mut self, job_id: &JobId, events: &mut Vec<ServiceEvent>) {
        let now = now_ms();
        if let Some(job) = self.find_job_mut(job_id) {
            let mut changed = false;
            for item in &mut job.items {
                if matches!(item.state, ItemState::Failed | ItemState::Cancelled) {
                    item.state = ItemState::Queued;
                    item.progress = ProgressState::pending("Queued for retry");
                    item.failure_reason = None;
                    item.outputs.clear();
                    item.flags.clear();
                    item.updated_at_ms = now;
                    item.started_at_ms = None;
                    item.finished_at_ms = None;
                    changed = true;
                }
            }
            if changed {
                job.state = JobState::Queued;
                job.error_message = None;
                job.progress = ProgressState::pending("Queued");
                job.updated_at_ms = now;
                Self::refresh_job_derived_state(job, None);
                events.push(ServiceEvent::JobUpdated(job.clone()));
            }
        }
    }

    fn requeue_history_job(&mut self, job_id: &JobId, events: &mut Vec<ServiceEvent>) {
        let Some(entry) = self
            .snapshot
            .history
            .iter()
            .find(|entry| &entry.job.id == job_id)
            .cloned()
        else {
            return;
        };

        let position = self.snapshot.queue.jobs.len() as u32;
        let now = now_ms();
        let job = Self::clone_history_job_for_queue(&entry, position, now);

        self.push_log(
            LogLevel::Info,
            LogScope::Service,
            Some(job.id.clone()),
            None,
            format!("Requeued history job {}", job.label),
            events,
        );
        events.push(ServiceEvent::JobQueued(job.clone()));
        self.snapshot.queue.jobs.push(job);
        self.reindex_queue();
    }

    fn remove_job(&mut self, job_id: &JobId, from_history: bool, events: &mut Vec<ServiceEvent>) {
        if from_history {
            let before = self.snapshot.history.len();
            self.snapshot
                .history
                .retain(|entry| &entry.job.id != job_id);
            if self.snapshot.history.len() != before {
                events.push(ServiceEvent::JobRemoved(job_id.clone()));
                events.push(ServiceEvent::HistoryUpdated(self.snapshot.history.clone()));
            }
            return;
        }

        if self.snapshot.queue.active_job_id.as_ref() == Some(job_id) {
            return;
        }

        let before = self.snapshot.queue.jobs.len();
        self.snapshot.queue.jobs.retain(|job| &job.id != job_id);
        if self.snapshot.queue.jobs.len() != before {
            self.reindex_queue();
            events.push(ServiceEvent::JobRemoved(job_id.clone()));
        }
    }

    fn reorder_job(&mut self, job_id: &JobId, new_position: u32, events: &mut Vec<ServiceEvent>) {
        let Some(old_index) = self
            .snapshot
            .queue
            .jobs
            .iter()
            .position(|job| &job.id == job_id)
        else {
            return;
        };

        let new_index = usize::min(
            new_position as usize,
            self.snapshot.queue.jobs.len().saturating_sub(1),
        );
        if new_index == old_index {
            return;
        }

        let job = self.snapshot.queue.jobs.remove(old_index);
        self.snapshot.queue.jobs.insert(new_index, job);
        self.reindex_queue();
        if let Some(job) = self.snapshot.queue.jobs.get(new_index).cloned() {
            events.push(ServiceEvent::JobUpdated(job));
        }
    }

    fn set_queue_status(&mut self, status: QueueStatus, events: &mut Vec<ServiceEvent>) {
        if self.snapshot.queue.status != status {
            self.snapshot.queue.status = status;
            events.push(ServiceEvent::QueueStatusChanged(status));
        }
    }

    fn apply_settings_side_effects(
        &mut self,
        previous_settings: &AppSettings,
        events: &mut Vec<ServiceEvent>,
    ) {
        if !Self::backend_settings_changed(previous_settings, &self.snapshot.settings) {
            return;
        }

        if self.snapshot.queue.active_job_id.is_some() {
            let message =
                "Backend setting changes are persisted and will apply after the active job ends"
                    .to_string();
            self.snapshot.service_health.last_error = Some(message.clone());
            self.push_log(
                LogLevel::Warn,
                LogScope::Service,
                None,
                None,
                message.clone(),
                events,
            );
            events.push(ServiceEvent::ErrorRaised {
                message,
                job_id: None,
                item_id: None,
            });
            return;
        }

        self.backend = Self::backend_from_settings(&self.snapshot.settings);
        self.refresh_backend_health();
        self.push_log(
            LogLevel::Info,
            LogScope::Service,
            None,
            None,
            format!(
                "Switched backend to {}",
                self.snapshot.service_health.backend_name
            ),
            events,
        );
    }

    fn backend_settings_changed(previous: &AppSettings, current: &AppSettings) -> bool {
        previous.preferred_backend != current.preferred_backend
            || previous.external_backend_executable != current.external_backend_executable
    }

    fn refresh_backend_health(&mut self) -> BackendHealth {
        let health = self.backend.health();
        self.snapshot.service_health.backend_name = self.backend.name().to_string();
        self.snapshot.service_health.backend_ready = health.ready;
        self.snapshot.service_health.backend_status_message = health.message.clone();

        if health.ready {
            self.snapshot.service_health.last_error = None;
        } else {
            self.snapshot.service_health.last_error = Some(health.message.clone());
        }

        health
    }

    fn report_backend_preflight_failure(
        &mut self,
        job_id: Option<JobId>,
        message: String,
        events: &mut Vec<ServiceEvent>,
    ) {
        self.snapshot.service_health.last_error = Some(message.clone());
        self.set_queue_status(QueueStatus::Paused, events);
        self.push_log(
            LogLevel::Warn,
            LogScope::Service,
            job_id.clone(),
            None,
            message.clone(),
            events,
        );
        events.push(ServiceEvent::ErrorRaised {
            message,
            job_id,
            item_id: None,
        });
    }

    fn effective_job_options(&self, options: Option<JobOptions>) -> JobOptions {
        let mut effective = options.unwrap_or_default();

        if effective.destination.trim().is_empty() {
            effective.destination = self.snapshot.settings.default_destination.clone();
        }

        if effective.format.trim().is_empty() {
            effective.format = self.snapshot.settings.default_format.clone();
        }

        if effective.max_parallel == 0 {
            effective.max_parallel = self.snapshot.settings.max_parallel;
        }

        effective
    }

    fn sanitize_settings(mut settings: AppSettings) -> AppSettings {
        settings.default_destination = settings.default_destination.trim().to_string();
        settings.default_format = if settings.default_format.trim().is_empty() {
            "flac".to_string()
        } else {
            settings.default_format.trim().to_string()
        };
        settings.max_parallel = settings.max_parallel.max(1);
        settings.max_history_entries = settings.max_history_entries.max(1);
        settings.max_log_entries = settings.max_log_entries.max(1);
        settings.external_backend_executable =
            settings.external_backend_executable.trim().to_string();
        settings
    }

    fn build_backend_request(job: &JobRecord) -> BackendRunRequest {
        BackendRunRequest {
            job_id: job.id.clone(),
            label: job.label.clone(),
            source_url: job.source_url.clone(),
            options: job.options.clone(),
            items: job
                .items
                .iter()
                .enumerate()
                .map(|(position, item)| BackendItemDescriptor {
                    item_id: item.id.clone(),
                    position: position as u32,
                    label: item.label.clone(),
                    url: item.url.clone(),
                    metadata: item.metadata.clone(),
                })
                .collect(),
        }
    }

    fn clone_history_job_for_queue(
        entry: &HistoryEntry,
        position: u32,
        now: TimestampMs,
    ) -> JobRecord {
        let mut job = entry.job.clone();
        job.id = JobId::new();
        job.position = position;
        job.state = JobState::Queued;
        job.progress = ProgressState::pending("Queued");
        job.created_at_ms = now;
        job.updated_at_ms = now;
        job.started_at_ms = None;
        job.finished_at_ms = None;
        job.error_message = None;

        for item in &mut job.items {
            item.id = spotifydl_protocol::ItemId::new();
            item.state = ItemState::Queued;
            item.progress = ProgressState::pending("Waiting for downloader");
            item.created_at_ms = now;
            item.updated_at_ms = now;
            item.started_at_ms = None;
            item.finished_at_ms = None;
            item.failure_reason = None;
            item.outputs.clear();
            item.flags.clear();
        }

        Self::refresh_job_derived_state(&mut job, None);
        job
    }

    fn refresh_job_derived_state(job: &mut JobRecord, collection: Option<&CollectionProgress>) {
        let mut totals = JobTotals {
            items_total: job.items.len() as u32,
            ..JobTotals::default()
        };

        let mut progress_current = 0u32;
        let mut progress_total = 0u32;
        let mut running_detail = None;
        let mut paused_detail = None;

        for item in &job.items {
            match item.state {
                ItemState::Completed => totals.items_completed += 1,
                ItemState::Failed => totals.items_failed += 1,
                ItemState::Cancelled => totals.items_cancelled += 1,
                ItemState::Queued | ItemState::Running | ItemState::Paused => {}
            }

            for output in &item.outputs {
                match output.disposition {
                    spotifydl_protocol::OutputDisposition::Written => totals.outputs_written += 1,
                    spotifydl_protocol::OutputDisposition::Replaced => totals.outputs_replaced += 1,
                    spotifydl_protocol::OutputDisposition::KeptBoth => totals.outputs_written += 1,
                    spotifydl_protocol::OutputDisposition::SkippedExisting => {
                        totals.outputs_skipped += 1
                    }
                    spotifydl_protocol::OutputDisposition::DeletedSmaller => {
                        totals.outputs_deleted += 1
                    }
                }
            }

            if !item.flags.is_empty() {
                totals.flagged_items += 1;
            }

            progress_current += u32::from(item.progress.percent);
            progress_total += 100;

            if running_detail.is_none() && item.state == ItemState::Running {
                running_detail = Some(item.progress.detail.clone());
            }
            if paused_detail.is_none() && item.state == ItemState::Paused {
                paused_detail = Some(item.progress.detail.clone());
            }
        }

        job.totals = totals;
        Self::refresh_job_label(job);
        job.progress = if let Some(progress) = Self::collection_progress_state(job, collection) {
            progress
        } else if progress_total == 0 {
            ProgressState::pending("Queued")
        } else if let Some(detail) = running_detail {
            ProgressState::from_steps(progress_current, progress_total, detail)
        } else if let Some(detail) = paused_detail {
            ProgressState::from_steps(progress_current, progress_total, detail)
        } else if job
            .items
            .iter()
            .all(|item| item.state == ItemState::Completed)
        {
            ProgressState::finished("Completed")
        } else if job
            .items
            .iter()
            .all(|item| matches!(item.state, ItemState::Completed | ItemState::Failed))
        {
            ProgressState::finished("Completed with failures")
        } else if job
            .items
            .iter()
            .all(|item| item.state == ItemState::Cancelled)
        {
            ProgressState::finished("Cancelled")
        } else {
            ProgressState::from_steps(progress_current, progress_total, "Queued")
        };
    }

    fn collection_progress_state(
        job: &JobRecord,
        collection: Option<&CollectionProgress>,
    ) -> Option<ProgressState> {
        if !Self::uses_collection_progress(job) {
            return None;
        }

        let collection = collection?;
        let item = job.items.first()?;

        let started_units = collection.started_units.max(1);
        let total_units = if job.state.is_terminal() {
            started_units.max(collection.finished_units).max(1)
        } else {
            started_units.max(collection.finished_units.saturating_add(1))
        };
        let current_percent = if item.state == ItemState::Running {
            u32::from(item.progress.percent)
        } else {
            0
        };
        let current = collection.finished_units.saturating_mul(100) + current_percent;
        let total = total_units.saturating_mul(100).max(100);
        let detail = if job.state.is_terminal() {
            job.progress.detail.clone()
        } else if item.state == ItemState::Running {
            item.progress.detail.clone()
        } else if job.state == JobState::Paused {
            "Paused".to_string()
        } else {
            "Queued".to_string()
        };

        Some(ProgressState::from_steps(current, total, detail))
    }

    fn uses_collection_progress(job: &JobRecord) -> bool {
        job.items.len() == 1
            && (job.source_url.contains("/album/")
                || job.source_url.contains("/playlist/")
                || job.source_url.contains("/artist/"))
    }

    fn collection_progress_for_start(
        map: &mut HashMap<JobId, CollectionProgress>,
        job: &JobRecord,
        label: Option<&str>,
    ) -> Option<CollectionProgress> {
        if !Self::uses_collection_progress(job) {
            return None;
        }

        let key = job.id.clone();
        let state = map.entry(key.clone()).or_default();
        let normalized_label = label
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        if normalized_label != state.current_unit_label {
            state.started_units = state
                .started_units
                .max(state.finished_units)
                .saturating_add(1);
            state.current_unit_label = normalized_label;
        }

        Some(state.clone())
    }

    fn collection_progress_for_finish(
        map: &mut HashMap<JobId, CollectionProgress>,
        job: &JobRecord,
    ) -> Option<CollectionProgress> {
        if !Self::uses_collection_progress(job) {
            return None;
        }

        let state = map.entry(job.id.clone()).or_default();
        if state.finished_units < state.started_units {
            state.finished_units += 1;
        } else {
            state.started_units = state.started_units.saturating_add(1);
            state.finished_units = state.started_units;
        }
        state.current_unit_label = None;
        Some(state.clone())
    }

    fn refresh_job_label(job: &mut JobRecord) {
        if !looks_like_spotify_url(&job.label)
            && job.label != "Album download"
            && job.label != "Playlist download"
        {
            return;
        }

        let source_url = job.source_url.as_str();
        if source_url.contains("/track/") {
            if let Some(item) = job.items.first() {
                if !item.metadata.title.trim().is_empty() {
                    let artist = if !item.metadata.artist.trim().is_empty() {
                        item.metadata.artist.trim()
                    } else {
                        item.metadata.album_artist.trim()
                    };
                    job.label = if artist.is_empty() {
                        item.metadata.title.trim().to_string()
                    } else {
                        format!("{} - {}", item.metadata.title.trim(), artist)
                    };
                    return;
                }

                if !looks_like_spotify_url(&item.label) && !item.label.trim().is_empty() {
                    job.label = item.label.trim().to_string();
                }
            }
            return;
        }

        if source_url.contains("/album/") {
            if let Some((album, artist)) = album_label(job) {
                job.label = if artist.is_empty() {
                    album
                } else {
                    format!("{album} - {artist}")
                };
            } else {
                job.label = "Album download".to_string();
            }
            return;
        }

        if source_url.contains("/playlist/") {
            job.label = "Playlist download".to_string();
        }
    }

    fn terminal_detail_for_item(state: ItemState) -> &'static str {
        match state {
            ItemState::Completed => "Completed",
            ItemState::Failed => "Failed",
            ItemState::Cancelled => "Cancelled",
            ItemState::Queued => "Queued",
            ItemState::Running => "Running",
            ItemState::Paused => "Paused",
        }
    }

    fn terminal_detail_for_job(state: JobState) -> &'static str {
        match state {
            JobState::Completed => "Completed",
            JobState::Failed => "Failed",
            JobState::Cancelled => "Cancelled",
            JobState::Queued => "Queued",
            JobState::Running => "Running",
            JobState::Paused => "Paused",
        }
    }
}

fn now_ms() -> TimestampMs {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

fn looks_like_spotify_url(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("https://open.spotify.com/")
        || trimmed.starts_with("spotify:")
        || trimmed.starts_with("http://open.spotify.com/")
}

fn album_label(job: &JobRecord) -> Option<(String, String)> {
    let album = job.items.iter().find_map(|item| {
        let album = item.metadata.album.trim();
        if album.is_empty() {
            None
        } else {
            Some(album.to_string())
        }
    })?;

    let artist = job
        .items
        .iter()
        .find_map(|item| {
            let artist = if item.metadata.album_artist.trim().is_empty() {
                item.metadata.artist.trim()
            } else {
                item.metadata.album_artist.trim()
            };

            if artist.is_empty() {
                None
            } else {
                Some(artist.to_string())
            }
        })
        .unwrap_or_default();

    Some((album, artist))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        io::Write,
        path::PathBuf,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use spotifydl_core::{BackendCapabilities, BackendHealth};
    use spotifydl_protocol::{
        BackendKind, BackoffState, IntegrityFlag, IntegritySeverity, ItemMetadata, OutputDisposition,
        OutputRecord, ServiceCommand,
    };

    #[derive(Default)]
    struct ScriptedEvents {
        start: VecDeque<Vec<BackendEvent>>,
        control: VecDeque<Vec<BackendEvent>>,
        tick: VecDeque<Vec<BackendEvent>>,
        control_requests: Vec<BackendControlRequest>,
    }

    #[derive(Clone, Default)]
    struct ScriptedBackendHandle {
        state: Arc<Mutex<ScriptedEvents>>,
    }

    impl ScriptedBackendHandle {
        fn push_start(&self, events: Vec<BackendEvent>) {
            self.state
                .lock()
                .expect("lock start scripts")
                .start
                .push_back(events);
        }

        fn push_tick(&self, events: Vec<BackendEvent>) {
            self.state
                .lock()
                .expect("lock tick scripts")
                .tick
                .push_back(events);
        }

        fn control_requests(&self) -> Vec<BackendControlRequest> {
            self.state
                .lock()
                .expect("lock control requests")
                .control_requests
                .clone()
        }
    }

    struct ScriptedBackend {
        handle: ScriptedBackendHandle,
        capabilities: BackendCapabilities,
        health: BackendHealth,
    }

    impl ScriptedBackend {
        fn with_capabilities(
            handle: ScriptedBackendHandle,
            capabilities: BackendCapabilities,
        ) -> Self {
            Self {
                handle,
                capabilities,
                health: BackendHealth::default(),
            }
        }

        fn with_health(handle: ScriptedBackendHandle, health: BackendHealth) -> Self {
            Self {
                handle,
                capabilities: BackendCapabilities::default(),
                health,
            }
        }
    }

    impl DownloadBackend for ScriptedBackend {
        fn name(&self) -> &'static str {
            "scripted-backend"
        }

        fn health(&self) -> BackendHealth {
            self.health.clone()
        }

        fn capabilities(&self) -> BackendCapabilities {
            self.capabilities
        }

        fn start(&mut self, _request: BackendRunRequest) -> Vec<BackendEvent> {
            self.handle
                .state
                .lock()
                .expect("lock start scripts")
                .start
                .pop_front()
                .unwrap_or_default()
        }

        fn control(&mut self, request: BackendControlRequest) -> Vec<BackendEvent> {
            let mut state = self.handle.state.lock().expect("lock control scripts");
            state.control_requests.push(request);
            state.control.pop_front().unwrap_or_default()
        }

        fn tick(&mut self) -> Vec<BackendEvent> {
            self.handle
                .state
                .lock()
                .expect("lock tick scripts")
                .tick
                .pop_front()
                .unwrap_or_default()
        }
    }

    #[test]
    fn reopens_with_persisted_backend_selection() {
        let database_path = temp_database_path();
        let mut service = SpotifydlService::open(&database_path).expect("service should open");
        let mut settings = service.snapshot().settings.clone();
        settings.preferred_backend = BackendKind::External;
        settings.external_backend_executable = "spotify-dl".to_string();

        service
            .dispatch(ServiceCommand::UpdateSettings { settings })
            .expect("settings update should persist");
        drop(service);

        let reopened = SpotifydlService::open(&database_path).expect("service should reopen");
        assert_eq!(
            reopened.snapshot().service_health.backend_name,
            "external-downloader"
        );
        drop(reopened);

        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn marks_external_backend_unready_when_executable_path_is_missing() {
        let database_path = temp_database_path();
        let mut service = SpotifydlService::open(&database_path).expect("service should open");
        let mut settings = service.snapshot().settings.clone();
        settings.preferred_backend = BackendKind::External;
        settings.external_backend_executable = "C:/definitely-missing/spotify-dl.exe".to_string();

        dispatch_ok(&mut service, ServiceCommand::UpdateSettings { settings });

        assert_eq!(
            service.snapshot().service_health.backend_name,
            "external-downloader"
        );
        assert!(!service.snapshot().service_health.backend_ready);
        assert!(
            service
                .snapshot()
                .service_health
                .backend_status_message
                .contains("not found")
        );

        drop(service);

        let reopened = SpotifydlService::open(&database_path).expect("service should reopen");
        assert!(!reopened.snapshot().service_health.backend_ready);
        assert!(
            reopened
                .snapshot()
                .service_health
                .backend_status_message
                .contains("not found")
        );
        drop(reopened);

        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn recovers_persisted_active_run_into_paused_state_on_reopen() {
        let database_path = temp_database_path();
        let handle = ScriptedBackendHandle::default();
        let mut service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ScriptedBackend::with_capabilities(
                handle.clone(),
                BackendCapabilities::default(),
            )),
        )
        .expect("service should open with scripted backend");

        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/recover-1"]);
        let item_id = item_ids[0].clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                label: Some("Recover Track".to_string()),
                metadata: None,
            },
        ]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);

        let mut settings = service.snapshot().settings.clone();
        settings.preferred_backend = BackendKind::External;
        settings.external_backend_executable = "C:/definitely-missing/spotify-dl.exe".to_string();
        dispatch_ok(&mut service, ServiceCommand::UpdateSettings { settings });

        let active_before = service
            .snapshot()
            .queue
            .active_job_id
            .clone()
            .expect("job should be active before reopen");
        assert_eq!(active_before, job_id);
        assert_eq!(service.snapshot().queue.jobs[0].state, JobState::Running);
        assert_eq!(
            service.snapshot().queue.jobs[0].items[0].state,
            ItemState::Running
        );

        drop(service);

        let reopened = SpotifydlService::open(&database_path).expect("service should reopen");
        let snapshot = reopened.snapshot();
        let recovered_job = &snapshot.queue.jobs[0];
        let recovered_item = &recovered_job.items[0];

        assert_eq!(snapshot.queue.status, QueueStatus::Paused);
        assert!(snapshot.queue.active_job_id.is_none());
        assert_eq!(recovered_job.id, job_id);
        assert_eq!(recovered_job.state, JobState::Paused);
        assert_eq!(
            recovered_job.progress.detail,
            "Recovered as paused after restart"
        );
        assert_eq!(recovered_item.id, item_id);
        assert_eq!(recovered_item.state, ItemState::Paused);
        assert_eq!(
            recovered_item.progress.detail,
            "Recovered as paused after restart"
        );
        assert!(snapshot.service_health.last_recovery_at_ms.is_some());
        assert!(snapshot.logs.iter().any(
            |entry| entry.message == "Recovered persisted runtime state and paused active jobs"
        ));

        drop(reopened);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn clears_persisted_backoff_when_recovering_on_reopen() {
        let database_path = temp_database_path();
        let handle = ScriptedBackendHandle::default();
        let mut service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ScriptedBackend::with_capabilities(
                handle.clone(),
                BackendCapabilities::default(),
            )),
        )
        .expect("service should open with scripted backend");

        let (job_id, item_ids) = enqueue_urls(
            &mut service,
            &["https://open.spotify.com/track/recover-backoff-1"],
        );
        let item_id = item_ids[0].clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id,
                label: Some("Recover Backoff Track".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![BackendEvent::BackoffStarted {
            job_id: Some(job_id.clone()),
            state: BackoffState {
                active: true,
                reason: "rate limited".to_string(),
                delay_ms: 4_000,
                remaining_ms: 4_000,
            },
        }]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        dispatch_ok(&mut service, ServiceCommand::Tick);

        assert_eq!(service.snapshot().queue.status, QueueStatus::Backoff);
        assert!(service.snapshot().service_health.backoff.active);
        assert_eq!(
            service.snapshot().service_health.backoff.remaining_ms,
            4_000
        );

        drop(service);

        let reopened = SpotifydlService::open(&database_path).expect("service should reopen");
        let snapshot = reopened.snapshot();
        let recovered_job = &snapshot.queue.jobs[0];

        assert_eq!(snapshot.queue.status, QueueStatus::Paused);
        assert!(snapshot.queue.active_job_id.is_none());
        assert_eq!(recovered_job.id, job_id);
        assert_eq!(recovered_job.state, JobState::Paused);
        assert!(!snapshot.service_health.backoff.active);
        assert_eq!(snapshot.service_health.backoff.remaining_ms, 0);
        assert_eq!(snapshot.service_health.backoff.reason, "rate limited");
        assert_eq!(snapshot.service_health.backoff.delay_ms, 4_000);
        assert!(snapshot.service_health.last_recovery_at_ms.is_some());

        drop(reopened);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn preserves_terminal_item_state_details_when_recovering_active_job() {
        let database_path = temp_database_path();
        let handle = ScriptedBackendHandle::default();
        let mut service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ScriptedBackend::with_capabilities(
                handle.clone(),
                BackendCapabilities::default(),
            )),
        )
        .expect("service should open with scripted backend");

        let (job_id, item_ids) = enqueue_urls(
            &mut service,
            &[
                "https://open.spotify.com/track/recover-failed-1",
                "https://open.spotify.com/track/recover-failed-2",
            ],
        );
        let failed_item_id = item_ids[0].clone();
        let active_item_id = item_ids[1].clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: failed_item_id.clone(),
                label: Some("Recover Failed Track".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![
            BackendEvent::ItemFinished {
                job_id: job_id.clone(),
                item_id: failed_item_id.clone(),
                state: ItemState::Failed,
                failure_reason: Some(FailureReason {
                    kind: FailureReasonKind::Network,
                    code: Some("NETWORK".to_string()),
                    message: "network timeout".to_string(),
                    details: None,
                }),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: active_item_id.clone(),
                label: Some("Recover Running Track".to_string()),
                metadata: None,
            },
        ]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        dispatch_ok(&mut service, ServiceCommand::Tick);

        let before_reopen = &service.snapshot().queue.jobs[0];
        assert_eq!(before_reopen.items[0].state, ItemState::Failed);
        assert_eq!(before_reopen.items[0].progress.detail, "Failed");
        assert_eq!(before_reopen.items[1].state, ItemState::Running);

        drop(service);

        let reopened = SpotifydlService::open(&database_path).expect("service should reopen");
        let snapshot = reopened.snapshot();
        let recovered_job = &snapshot.queue.jobs[0];
        let recovered_failed_item = recovered_job
            .items
            .iter()
            .find(|item| item.id == failed_item_id)
            .expect("failed item should still exist");
        let recovered_active_item = recovered_job
            .items
            .iter()
            .find(|item| item.id == active_item_id)
            .expect("active item should still exist");

        assert_eq!(snapshot.queue.status, QueueStatus::Paused);
        assert!(snapshot.queue.active_job_id.is_none());
        assert_eq!(recovered_job.id, job_id);
        assert_eq!(recovered_job.state, JobState::Paused);
        assert_eq!(recovered_failed_item.state, ItemState::Failed);
        assert_eq!(recovered_failed_item.progress.detail, "Failed");
        assert_eq!(
            recovered_failed_item
                .failure_reason
                .as_ref()
                .expect("failed item reason")
                .message,
            "network timeout"
        );
        assert_eq!(recovered_active_item.state, ItemState::Paused);
        assert_eq!(
            recovered_active_item.progress.detail,
            "Recovered as paused after restart"
        );

        drop(reopened);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn maps_real_external_backend_process_events_into_history_state() {
        let database_path = temp_database_path();
        let temp_root = temp_fixture_dir("external-smoke");
        let script_path = write_external_script(
            &temp_root,
            "external-smoke.cmd",
            &[
                r#"echo {"event":"track_start","track_id":"smoke1","track":"Smoke Track 1"}"#,
                r#"echo {"event":"stage","track_id":"smoke1","track":"Smoke Track 1","stage":"download","status":"progress","progress":50}"#,
                r#"echo {"event":"track_complete","track_id":"smoke1","track":"Smoke Track 1","path":"C:/music/smoke1.flac"}"#,
                r#"echo {"event":"track_start","track_id":"smoke2","track":"Smoke Track 2"}"#,
                r#"echo {"event":"track_failed","track_id":"smoke2","track":"Smoke Track 2","reason":"network timeout"}"#,
            ],
            0,
        );
        let comspec = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .expect("ComSpec should point to cmd.exe");

        let mut service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ExternalDownloader::new(ExternalDownloaderConfig {
                executable: comspec,
                working_directory: Some(temp_root.clone()),
                base_args: vec!["/C".to_string(), script_path.to_string_lossy().to_string()],
                environment: Vec::new(),
            })),
        )
        .expect("service should open with external backend");

        let (job_id, item_ids) = enqueue_urls(
            &mut service,
            &[
                "https://open.spotify.com/track/smoke1",
                "https://open.spotify.com/track/smoke2",
            ],
        );

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        pump_ticks_until(
            &mut service,
            |service| {
                service
                    .snapshot()
                    .history
                    .iter()
                    .any(|entry| entry.job.id == job_id)
            },
            80,
        );

        let history_job = service
            .snapshot()
            .history
            .iter()
            .find(|entry| entry.job.id == job_id)
            .expect("job should move to history")
            .job
            .clone();

        assert_eq!(history_job.state, JobState::Failed);
        assert_eq!(history_job.totals.items_completed, 1);
        assert_eq!(history_job.totals.items_failed, 1);
        assert_eq!(history_job.totals.outputs_written, 1);
        assert_eq!(history_job.items.len(), 2);

        let completed_item = history_job
            .items
            .iter()
            .find(|item| item.id == item_ids[0])
            .expect("completed item should exist");
        assert_eq!(completed_item.state, ItemState::Completed);
        assert_eq!(completed_item.outputs.len(), 1);
        assert_eq!(completed_item.outputs[0].final_path, "C:/music/smoke1.flac");

        let failed_item = history_job
            .items
            .iter()
            .find(|item| item.id == item_ids[1])
            .expect("failed item should exist");
        assert_eq!(failed_item.state, ItemState::Failed);
        assert_eq!(
            failed_item
                .failure_reason
                .as_ref()
                .expect("failed item reason")
                .message,
            "network timeout"
        );

        assert!(
            service
                .snapshot()
                .logs
                .iter()
                .any(|entry| { entry.message.contains("Launching external downloader:") }),
            "expected launch log from external backend"
        );

        drop(service);
        let _ = fs::remove_file(&database_path);
        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn maps_real_external_backoff_and_skip_events() {
        let database_path = temp_database_path();
        let temp_root = temp_fixture_dir("external-backoff-skip");
        let script_path = write_external_script(
            &temp_root,
            "external-backoff-skip.cmd",
            &[
                r#"echo {"event":"track_start","track_id":"skip1","track":"Skip Track"}"#,
                r#"echo {"event":"rate_limit_backoff","reason":"rate limited","delay_ms":1500}"#,
                r#"echo {"event":"rate_limit_wait","waited_ms":1500}"#,
                r#"echo {"event":"track_skipped","track_id":"skip1","track":"Skip Track"}"#,
            ],
            0,
        );
        let comspec = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .expect("ComSpec should point to cmd.exe");

        let mut service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ExternalDownloader::new(ExternalDownloaderConfig {
                executable: comspec,
                working_directory: Some(temp_root.clone()),
                base_args: vec!["/C".to_string(), script_path.to_string_lossy().to_string()],
                environment: Vec::new(),
            })),
        )
        .expect("service should open with external backend");

        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/skip1"]);
        let item_id = item_ids[0].clone();
        let mut saw_backoff = false;

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        for _ in 0..80 {
            if service
                .snapshot()
                .history
                .iter()
                .any(|entry| entry.job.id == job_id)
            {
                break;
            }

            let events = dispatch_ok(&mut service, ServiceCommand::Tick);
            if events
                .iter()
                .any(|event| matches!(event, ServiceEvent::BackoffStarted(_)))
            {
                saw_backoff = true;
            }
            thread::sleep(Duration::from_millis(20));
        }

        assert!(
            saw_backoff,
            "expected queue to enter backoff during external run"
        );

        let history_job = service
            .snapshot()
            .history
            .iter()
            .find(|entry| entry.job.id == job_id)
            .expect("job should move to history")
            .job
            .clone();

        assert_eq!(history_job.state, JobState::Completed);
        assert_eq!(history_job.totals.items_completed, 1);
        assert_eq!(history_job.totals.outputs_written, 0);
        assert_eq!(history_job.totals.flagged_items, 1);
        assert!(!service.snapshot().service_health.backoff.active);

        let item = history_job
            .items
            .iter()
            .find(|item| item.id == item_id)
            .expect("history item should exist");
        assert_eq!(item.state, ItemState::Completed);
        assert!(item.outputs.is_empty());
        assert_eq!(item.flags.len(), 1);
        assert_eq!(item.flags[0].kind, "skip");

        drop(service);
        let _ = fs::remove_file(&database_path);
        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn maps_real_external_process_exit_failure_into_failed_history_job() {
        let database_path = temp_database_path();
        let temp_root = temp_fixture_dir("external-exit-failure");
        let script_path = write_external_script(
            &temp_root,
            "external-exit-failure.cmd",
            &[r#"echo {"event":"track_start","track_id":"exit1","track":"Exit Failure Track"}"#],
            7,
        );
        let comspec = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .expect("ComSpec should point to cmd.exe");

        let mut service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ExternalDownloader::new(ExternalDownloaderConfig {
                executable: comspec,
                working_directory: Some(temp_root.clone()),
                base_args: vec!["/C".to_string(), script_path.to_string_lossy().to_string()],
                environment: Vec::new(),
            })),
        )
        .expect("service should open with external backend");

        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/exit1"]);
        let item_id = item_ids[0].clone();

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        pump_ticks_until(
            &mut service,
            |service| {
                service
                    .snapshot()
                    .history
                    .iter()
                    .any(|entry| entry.job.id == job_id)
            },
            80,
        );

        let history_job = service
            .snapshot()
            .history
            .iter()
            .find(|entry| entry.job.id == job_id)
            .expect("job should move to history")
            .job
            .clone();

        assert_eq!(history_job.state, JobState::Failed);
        assert_eq!(history_job.totals.items_failed, 1);
        assert_eq!(
            history_job.error_message.as_deref(),
            Some("External downloader exited unsuccessfully")
        );

        let item = history_job
            .items
            .iter()
            .find(|item| item.id == item_id)
            .expect("history item should exist");
        assert_eq!(item.state, ItemState::Failed);
        assert_eq!(
            item.failure_reason
                .as_ref()
                .and_then(|reason| reason.code.as_deref()),
            Some("EXIT_7")
        );
        assert!(
            service.snapshot().logs.iter().any(|entry| {
                entry
                    .message
                    .contains("External downloader exited with status Some(7)")
            }),
            "expected exit-status log from external backend"
        );

        drop(service);
        let _ = fs::remove_file(&database_path);
        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn maps_real_external_malformed_stdout_and_stderr_into_logs() {
        let database_path = temp_database_path();
        let temp_root = temp_fixture_dir("external-log-noise");
        let script_path = write_external_script(
            &temp_root,
            "external-log-noise.cmd",
            &[
                r#"echo not-json-output"#,
                r#"echo {"event":"track_start","track_id":"noise1","track":"Noise Track"}"#,
                r#"echo {"event":"track_complete","track_id":"noise1","track":"Noise Track","path":"C:/music/noise1.flac"}"#,
                r#"echo stderr: transient warning 1>&2"#,
            ],
            0,
        );
        let comspec = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .expect("ComSpec should point to cmd.exe");

        let mut service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ExternalDownloader::new(ExternalDownloaderConfig {
                executable: comspec,
                working_directory: Some(temp_root.clone()),
                base_args: vec!["/C".to_string(), script_path.to_string_lossy().to_string()],
                environment: Vec::new(),
            })),
        )
        .expect("service should open with external backend");

        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/noise1"]);
        let item_id = item_ids[0].clone();

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        pump_ticks_until(
            &mut service,
            |service| {
                service
                    .snapshot()
                    .history
                    .iter()
                    .any(|entry| entry.job.id == job_id)
            },
            80,
        );

        let history_job = service
            .snapshot()
            .history
            .iter()
            .find(|entry| entry.job.id == job_id)
            .expect("job should move to history")
            .job
            .clone();
        let item = history_job
            .items
            .iter()
            .find(|item| item.id == item_id)
            .expect("history item should exist");

        assert_eq!(history_job.state, JobState::Completed);
        assert_eq!(item.state, ItemState::Completed);
        assert_eq!(item.outputs.len(), 1);
        assert!(
            service
                .snapshot()
                .logs
                .iter()
                .any(|entry| { entry.message.contains("[stdout] not-json-output") }),
            "expected malformed stdout line to be preserved in logs"
        );
        assert!(
            service
                .snapshot()
                .logs
                .iter()
                .any(|entry| { entry.message.contains("[stderr] stderr: transient warning") }),
            "expected stderr output to be preserved in logs"
        );

        drop(service);
        let _ = fs::remove_file(&database_path);
        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn logs_unmatched_external_track_events_without_corrupting_completion() {
        let database_path = temp_database_path();
        let temp_root = temp_fixture_dir("external-unmatched");
        let script_path = write_external_script(
            &temp_root,
            "external-unmatched.cmd",
            &[
                r#"echo {"event":"track_start","track_id":"unknown-track","track":"Unknown Track"}"#,
                r#"echo {"event":"track_start","track_id":"known1","track":"Known Track"}"#,
                r#"echo {"event":"track_complete","track_id":"known1","track":"Known Track","path":"C:/music/known1.flac"}"#,
                r#"echo {"event":"track_start","track_id":"known2","track":"Known Track 2"}"#,
                r#"echo {"event":"track_complete","track_id":"known2","track":"Known Track 2","path":"C:/music/known2.flac"}"#,
            ],
            0,
        );
        let comspec = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .expect("ComSpec should point to cmd.exe");

        let mut service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ExternalDownloader::new(ExternalDownloaderConfig {
                executable: comspec,
                working_directory: Some(temp_root.clone()),
                base_args: vec!["/C".to_string(), script_path.to_string_lossy().to_string()],
                environment: Vec::new(),
            })),
        )
        .expect("service should open with external backend");

        let (job_id, item_ids) = enqueue_urls(
            &mut service,
            &[
                "https://open.spotify.com/track/known1",
                "https://open.spotify.com/track/known2",
            ],
        );
        let item_id_1 = item_ids[0].clone();
        let item_id_2 = item_ids[1].clone();

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        pump_ticks_until(
            &mut service,
            |service| {
                service
                    .snapshot()
                    .history
                    .iter()
                    .any(|entry| entry.job.id == job_id)
            },
            80,
        );

        let history_job = service
            .snapshot()
            .history
            .iter()
            .find(|entry| entry.job.id == job_id)
            .expect("job should move to history")
            .job
            .clone();
        let item_1 = history_job
            .items
            .iter()
            .find(|item| item.id == item_id_1)
            .expect("first history item should exist");
        let item_2 = history_job
            .items
            .iter()
            .find(|item| item.id == item_id_2)
            .expect("second history item should exist");

        assert_eq!(history_job.state, JobState::Completed);
        assert_eq!(item_1.state, ItemState::Completed);
        assert_eq!(item_1.outputs.len(), 1);
        assert_eq!(item_2.state, ItemState::Completed);
        assert_eq!(item_2.outputs.len(), 1);
        assert!(
            service
                .snapshot()
                .logs
                .iter()
                .any(|entry| { entry.message.contains("Unmatched track_start event") }),
            "expected unmatched track event to be logged"
        );

        drop(service);
        let _ = fs::remove_file(&database_path);
        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn completes_successful_job_with_output_history() {
        let (mut service, handle, database_path) = open_scripted_service();
        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/success1"]);
        let item_id = item_ids[0].clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                label: Some("Success Track".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![BackendEvent::ItemProgress {
            job_id: job_id.clone(),
            item_id: item_id.clone(),
            progress: ProgressState::from_steps(50, 100, "downloading | progress | Success Track"),
        }]);
        handle.push_tick(vec![
            BackendEvent::ItemOutput {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                output: OutputRecord {
                    disposition: OutputDisposition::Written,
                    final_path: "C:/music/success-track.flac".to_string(),
                    artist: "Artist".to_string(),
                    album: "Album".to_string(),
                    title: "Success Track".to_string(),
                    size_bytes: 42,
                    format: "flac".to_string(),
                    details: None,
                },
            },
            BackendEvent::ItemFinished {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                state: ItemState::Completed,
                failure_reason: None,
            },
            BackendEvent::JobFinished {
                job_id: job_id.clone(),
                state: JobState::Completed,
                failure_reason: None,
            },
        ]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        dispatch_ok(&mut service, ServiceCommand::Tick);
        dispatch_ok(&mut service, ServiceCommand::Tick);

        assert!(service.snapshot().queue.jobs.is_empty());
        assert_eq!(service.snapshot().history.len(), 1);
        let job = &service.snapshot().history[0].job;
        assert_eq!(job.state, JobState::Completed);
        assert_eq!(job.totals.items_completed, 1);
        assert_eq!(job.totals.outputs_written, 1);
        assert_eq!(job.items[0].outputs.len(), 1);
        assert_eq!(
            job.items[0].outputs[0].final_path,
            "C:/music/success-track.flac"
        );

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn keeps_collection_progress_monotonic_across_track_boundaries() {
        let (mut service, handle, database_path) = open_scripted_service();
        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/album/demo-album"]);
        let item_id = item_ids[0].clone();
        let metadata = ItemMetadata {
            artist: "Demo Artist".to_string(),
            album: "Demo Album".to_string(),
            title: "Track One".to_string(),
            album_artist: "Demo Artist".to_string(),
        };

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                label: Some("Track One".to_string()),
                metadata: Some(metadata.clone()),
            },
        ]);
        handle.push_tick(vec![BackendEvent::ItemProgress {
            job_id: job_id.clone(),
            item_id: item_id.clone(),
            progress: ProgressState::from_steps(50, 100, "downloading track one"),
        }]);
        handle.push_tick(vec![BackendEvent::ItemFinished {
            job_id: job_id.clone(),
            item_id: item_id.clone(),
            state: ItemState::Completed,
            failure_reason: None,
        }]);
        handle.push_tick(vec![BackendEvent::ItemStarted {
            job_id: job_id.clone(),
            item_id: item_id.clone(),
            label: Some("Track Two".to_string()),
            metadata: Some(ItemMetadata {
                title: "Track Two".to_string(),
                ..metadata.clone()
            }),
        }]);
        handle.push_tick(vec![BackendEvent::ItemProgress {
            job_id: job_id.clone(),
            item_id: item_id.clone(),
            progress: ProgressState::from_steps(50, 100, "downloading track two"),
        }]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        let started_job = service
            .snapshot()
            .queue
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .expect("started job");
        assert_eq!(started_job.label, "Demo Album - Demo Artist");

        dispatch_ok(&mut service, ServiceCommand::Tick);
        let first_track_mid = service
            .snapshot()
            .queue
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .expect("first track progress")
            .progress
            .percent;
        assert_eq!(first_track_mid, 50);

        dispatch_ok(&mut service, ServiceCommand::Tick);
        let after_first_finish = service
            .snapshot()
            .queue
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .expect("after first finish")
            .progress
            .percent;
        assert_eq!(after_first_finish, 50);

        dispatch_ok(&mut service, ServiceCommand::Tick);
        let second_start = service
            .snapshot()
            .queue
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .expect("second start")
            .progress
            .percent;
        assert_eq!(second_start, 50);

        dispatch_ok(&mut service, ServiceCommand::Tick);
        let second_track_mid = service
            .snapshot()
            .queue
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .expect("second track progress")
            .progress
            .percent;
        assert_eq!(second_track_mid, 75);

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn preserves_partial_success_when_job_finishes_failed() {
        let (mut service, handle, database_path) = open_scripted_service();
        let (job_id, item_ids) = enqueue_urls(
            &mut service,
            &[
                "https://open.spotify.com/track/first",
                "https://open.spotify.com/track/second",
            ],
        );
        let item_1 = item_ids[0].clone();
        let item_2 = item_ids[1].clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: item_1.clone(),
                label: Some("First Track".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![
            BackendEvent::ItemOutput {
                job_id: job_id.clone(),
                item_id: item_1.clone(),
                output: OutputRecord {
                    disposition: OutputDisposition::Written,
                    final_path: "C:/music/first.flac".to_string(),
                    artist: "Artist".to_string(),
                    album: "Album".to_string(),
                    title: "First Track".to_string(),
                    size_bytes: 128,
                    format: "flac".to_string(),
                    details: None,
                },
            },
            BackendEvent::ItemFinished {
                job_id: job_id.clone(),
                item_id: item_1.clone(),
                state: ItemState::Completed,
                failure_reason: None,
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: item_2.clone(),
                label: Some("Second Track".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![
            BackendEvent::ItemFinished {
                job_id: job_id.clone(),
                item_id: item_2.clone(),
                state: ItemState::Failed,
                failure_reason: Some(FailureReason {
                    kind: FailureReasonKind::Network,
                    code: Some("NETWORK".to_string()),
                    message: "network timeout".to_string(),
                    details: None,
                }),
            },
            BackendEvent::JobFinished {
                job_id: job_id.clone(),
                state: JobState::Failed,
                failure_reason: Some(FailureReason {
                    kind: FailureReasonKind::Unknown,
                    code: Some("ITEM_FAILURES_REPORTED".to_string()),
                    message: "one or more items failed".to_string(),
                    details: None,
                }),
            },
        ]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        dispatch_ok(&mut service, ServiceCommand::Tick);
        dispatch_ok(&mut service, ServiceCommand::Tick);

        let job = &service.snapshot().history[0].job;
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.totals.items_completed, 1);
        assert_eq!(job.totals.items_failed, 1);
        assert_eq!(job.totals.outputs_written, 1);
        assert_eq!(job.items[0].state, ItemState::Completed);
        assert_eq!(job.items[1].state, ItemState::Failed);
        assert_eq!(
            job.items[1]
                .failure_reason
                .as_ref()
                .expect("failed item reason")
                .message,
            "network timeout"
        );

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn records_skip_flags_without_fake_outputs() {
        let (mut service, handle, database_path) = open_scripted_service();
        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/skip1"]);
        let item_id = item_ids[0].clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                label: Some("Skip Track".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![
            BackendEvent::ItemFlag {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                flag: IntegrityFlag {
                    severity: IntegritySeverity::Info,
                    kind: "skip".to_string(),
                    message: "External downloader skipped existing output".to_string(),
                    details: Some("Skip Track".to_string()),
                },
            },
            BackendEvent::ItemFinished {
                job_id: job_id.clone(),
                item_id,
                state: ItemState::Completed,
                failure_reason: None,
            },
            BackendEvent::JobFinished {
                job_id,
                state: JobState::Completed,
                failure_reason: None,
            },
        ]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        dispatch_ok(&mut service, ServiceCommand::Tick);

        let job = &service.snapshot().history[0].job;
        assert_eq!(job.state, JobState::Completed);
        assert_eq!(job.totals.outputs_written, 0);
        assert_eq!(job.totals.flagged_items, 1);
        assert_eq!(job.items[0].flags.len(), 1);
        assert!(job.items[0].outputs.is_empty());

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn transitions_through_backoff_and_clears_it() {
        let (mut service, handle, database_path) = open_scripted_service();
        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/backoff1"]);
        let item_id = item_ids[0].clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                label: Some("Backoff Track".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![BackendEvent::BackoffStarted {
            job_id: Some(job_id.clone()),
            state: BackoffState {
                active: true,
                reason: "rate limited".to_string(),
                delay_ms: 1500,
                remaining_ms: 1500,
            },
        }]);
        handle.push_tick(vec![BackendEvent::BackoffTick {
            job_id: Some(job_id.clone()),
            state: BackoffState {
                active: false,
                reason: "rate limited".to_string(),
                delay_ms: 1500,
                remaining_ms: 0,
            },
        }]);
        handle.push_tick(vec![
            BackendEvent::ItemFinished {
                job_id: job_id.clone(),
                item_id,
                state: ItemState::Completed,
                failure_reason: None,
            },
            BackendEvent::JobFinished {
                job_id,
                state: JobState::Completed,
                failure_reason: None,
            },
        ]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        dispatch_ok(&mut service, ServiceCommand::Tick);
        assert_eq!(service.snapshot().queue.status, QueueStatus::Backoff);
        assert!(service.snapshot().service_health.backoff.active);

        dispatch_ok(&mut service, ServiceCommand::Tick);
        assert_eq!(service.snapshot().queue.status, QueueStatus::Running);
        assert!(!service.snapshot().service_health.backoff.active);

        dispatch_ok(&mut service, ServiceCommand::Tick);
        assert_eq!(service.snapshot().queue.status, QueueStatus::Idle);
        assert_eq!(service.snapshot().history[0].job.state, JobState::Completed);

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn requeues_history_job_as_fresh_queue_entry() {
        let (mut service, handle, database_path) = open_scripted_service();
        let (job_id, item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/requeue1"]);
        let original_item_id = item_ids[0].clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: job_id.clone(),
                item_id: original_item_id.clone(),
                label: Some("Requeue Track".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![
            BackendEvent::ItemFinished {
                job_id: job_id.clone(),
                item_id: original_item_id,
                state: ItemState::Failed,
                failure_reason: Some(FailureReason {
                    kind: FailureReasonKind::Network,
                    code: Some("NETWORK".to_string()),
                    message: "transient failure".to_string(),
                    details: None,
                }),
            },
            BackendEvent::JobFinished {
                job_id: job_id.clone(),
                state: JobState::Failed,
                failure_reason: Some(FailureReason {
                    kind: FailureReasonKind::Unknown,
                    code: Some("ITEM_FAILURES_REPORTED".to_string()),
                    message: "one or more items failed".to_string(),
                    details: None,
                }),
            },
        ]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        dispatch_ok(&mut service, ServiceCommand::Tick);
        let history_job = service.snapshot().history[0].job.clone();

        dispatch_ok(
            &mut service,
            ServiceCommand::RequeueHistoryJob {
                job_id: history_job.id.clone(),
            },
        );

        assert_eq!(service.snapshot().queue.jobs.len(), 1);
        let requeued = &service.snapshot().queue.jobs[0];
        assert_ne!(requeued.id, history_job.id);
        assert_eq!(requeued.state, JobState::Queued);
        assert_eq!(requeued.items.len(), 1);
        assert_eq!(requeued.items[0].state, ItemState::Queued);
        assert!(requeued.items[0].failure_reason.is_none());
        assert!(requeued.items[0].outputs.is_empty());
        assert!(requeued.items[0].flags.is_empty());
        assert_ne!(requeued.items[0].id, history_job.items[0].id);

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn pauses_non_pauseable_backend_at_job_boundary() {
        let (mut service, handle, database_path) =
            open_scripted_service_with_capabilities(BackendCapabilities::default());
        let (first_job_id, first_item_ids) =
            enqueue_urls(&mut service, &["https://open.spotify.com/track/boundary-1"]);
        let first_item_id = first_item_ids[0].clone();

        dispatch_ok(
            &mut service,
            ServiceCommand::EnqueueUrls {
                urls: vec!["https://open.spotify.com/track/boundary-2".to_string()],
                source: None,
                options: None,
            },
        );
        let second_job_id = service.snapshot().queue.jobs[1].id.clone();

        handle.push_start(vec![
            BackendEvent::JobStarted {
                job_id: first_job_id.clone(),
            },
            BackendEvent::ItemStarted {
                job_id: first_job_id.clone(),
                item_id: first_item_id.clone(),
                label: Some("Boundary Track 1".to_string()),
                metadata: None,
            },
        ]);
        handle.push_tick(vec![
            BackendEvent::ItemFinished {
                job_id: first_job_id.clone(),
                item_id: first_item_id,
                state: ItemState::Completed,
                failure_reason: None,
            },
            BackendEvent::JobFinished {
                job_id: first_job_id.clone(),
                state: JobState::Completed,
                failure_reason: None,
            },
        ]);
        handle.push_start(vec![BackendEvent::JobStarted {
            job_id: second_job_id.clone(),
        }]);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);
        dispatch_ok(&mut service, ServiceCommand::PauseQueue);

        assert_eq!(service.snapshot().queue.status, QueueStatus::Paused);
        assert_eq!(
            service
                .snapshot()
                .queue
                .active_job_id
                .as_ref()
                .expect("active job while draining"),
            &first_job_id
        );
        assert_eq!(service.snapshot().queue.jobs[0].state, JobState::Running);
        assert!(handle.control_requests().is_empty());

        dispatch_ok(&mut service, ServiceCommand::Tick);

        assert_eq!(service.snapshot().queue.status, QueueStatus::Paused);
        assert!(service.snapshot().queue.active_job_id.is_none());
        assert_eq!(service.snapshot().queue.jobs.len(), 1);
        assert_eq!(service.snapshot().queue.jobs[0].id, second_job_id);
        assert_eq!(service.snapshot().queue.jobs[0].state, JobState::Queued);
        assert_eq!(service.snapshot().history.len(), 1);
        assert_eq!(service.snapshot().history[0].job.id, first_job_id);

        dispatch_ok(&mut service, ServiceCommand::StartQueue);

        assert_eq!(
            service
                .snapshot()
                .queue
                .active_job_id
                .as_ref()
                .expect("second job should start after resume"),
            &second_job_id
        );

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn applies_saved_default_settings_to_new_jobs() {
        let (mut service, _handle, database_path) = open_scripted_service();
        let mut settings = service.snapshot().settings.clone();
        settings.default_destination = "C:/music".to_string();
        settings.default_format = "mp3".to_string();
        settings.max_parallel = 7;

        dispatch_ok(&mut service, ServiceCommand::UpdateSettings { settings });
        dispatch_ok(
            &mut service,
            ServiceCommand::EnqueueUrls {
                urls: vec!["https://open.spotify.com/track/defaults-1".to_string()],
                source: None,
                options: None,
            },
        );

        let job = &service.snapshot().queue.jobs[0];
        assert_eq!(job.options.destination, "C:/music");
        assert_eq!(job.options.format, "mp3");
        assert_eq!(job.options.max_parallel, 7);

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn normalizes_invalid_settings_values() {
        let (mut service, _handle, database_path) = open_scripted_service();
        let mut settings = service.snapshot().settings.clone();
        settings.default_destination = "  C:/music  ".to_string();
        settings.default_format = "   ".to_string();
        settings.max_parallel = 0;
        settings.max_history_entries = 0;
        settings.max_log_entries = 0;
        settings.external_backend_executable = "  spotify-dl  ".to_string();

        dispatch_ok(&mut service, ServiceCommand::UpdateSettings { settings });

        let saved = &service.snapshot().settings;
        assert_eq!(saved.default_destination, "C:/music");
        assert_eq!(saved.default_format, "flac");
        assert_eq!(saved.max_parallel, 1);
        assert_eq!(saved.max_history_entries, 1);
        assert_eq!(saved.max_log_entries, 1);
        assert_eq!(saved.external_backend_executable, "spotify-dl");

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    #[test]
    fn leaves_job_queued_when_backend_is_unready() {
        let (mut service, handle, database_path) =
            open_scripted_service_with_health(BackendHealth {
                ready: false,
                message: "Scripted backend is not configured".to_string(),
            });

        enqueue_urls(&mut service, &["https://open.spotify.com/track/unready-1"]);
        let events = dispatch_ok(&mut service, ServiceCommand::StartQueue);

        assert_eq!(service.snapshot().queue.status, QueueStatus::Paused);
        assert!(service.snapshot().queue.active_job_id.is_none());
        assert_eq!(service.snapshot().queue.jobs.len(), 1);
        assert_eq!(service.snapshot().queue.jobs[0].state, JobState::Queued);
        assert!(handle.control_requests().is_empty());
        assert!(!service.snapshot().service_health.backend_ready);
        assert_eq!(
            service.snapshot().service_health.backend_status_message,
            "Scripted backend is not configured"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ServiceEvent::ErrorRaised { message, .. }
                if message == "Scripted backend is not configured"
        )));

        drop(service);
        let _ = fs::remove_file(&database_path);
    }

    fn open_scripted_service() -> (SpotifydlService, ScriptedBackendHandle, PathBuf) {
        open_scripted_service_with_capabilities(BackendCapabilities::default())
    }

    fn open_scripted_service_with_health(
        health: BackendHealth,
    ) -> (SpotifydlService, ScriptedBackendHandle, PathBuf) {
        let database_path = temp_database_path();
        let handle = ScriptedBackendHandle::default();
        let service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ScriptedBackend::with_health(handle.clone(), health)),
        )
        .expect("scripted service should open");

        (service, handle, database_path)
    }

    fn open_scripted_service_with_capabilities(
        capabilities: BackendCapabilities,
    ) -> (SpotifydlService, ScriptedBackendHandle, PathBuf) {
        let database_path = temp_database_path();
        let handle = ScriptedBackendHandle::default();
        let service = SpotifydlService::open_with_backend(
            &database_path,
            Box::new(ScriptedBackend::with_capabilities(
                handle.clone(),
                capabilities,
            )),
        )
        .expect("scripted service should open");

        (service, handle, database_path)
    }

    fn dispatch_ok(service: &mut SpotifydlService, command: ServiceCommand) -> Vec<ServiceEvent> {
        service.dispatch(command).expect("command should succeed")
    }

    fn enqueue_urls(service: &mut SpotifydlService, urls: &[&str]) -> (JobId, Vec<ItemId>) {
        assert!(!urls.is_empty(), "enqueue_urls requires at least one URL");
        dispatch_ok(
            service,
            ServiceCommand::EnqueueUrls {
                urls: vec![urls[0].to_string()],
                source: None,
                options: None,
            },
        );
        let job_id = service.snapshot().queue.jobs[0].id.clone();

        if urls.len() > 1 {
            dispatch_ok(
                service,
                ServiceCommand::AddUrlsToJob {
                    job_id: job_id.clone(),
                    urls: urls[1..].iter().map(|url| (*url).to_string()).collect(),
                },
            );
        }

        let item_ids = service.snapshot().queue.jobs[0]
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect();

        (job_id, item_ids)
    }

    fn temp_database_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should advance")
            .as_nanos();
        std::env::temp_dir().join(format!("spotifydl-service-test-{unique}.sqlite"))
    }

    fn temp_fixture_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should advance")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(&path).expect("temp fixture dir should create");
        path
    }

    fn write_external_script(
        root: &PathBuf,
        file_name: &str,
        lines: &[&str],
        exit_code: i32,
    ) -> PathBuf {
        let script_path = root.join(file_name);
        let mut file = fs::File::create(&script_path).expect("smoke script should create");
        writeln!(file, "@echo off").expect("script should write");
        for line in lines {
            writeln!(file, "{line}").expect("script should write");
        }
        writeln!(file, "exit /b {exit_code}").expect("script should write");
        script_path
    }

    fn pump_ticks_until(
        service: &mut SpotifydlService,
        mut condition: impl FnMut(&SpotifydlService) -> bool,
        max_ticks: usize,
    ) {
        for _ in 0..max_ticks {
            if condition(service) {
                return;
            }

            dispatch_ok(service, ServiceCommand::Tick);
            thread::sleep(Duration::from_millis(20));
        }

        assert!(
            condition(service),
            "condition was not met within {max_ticks} ticks"
        );
    }
}
