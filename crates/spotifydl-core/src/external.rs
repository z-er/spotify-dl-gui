use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use serde::Deserialize;
use spotifydl_protocol::{
    BackoffState, FailureReason, FailureReasonKind, IntegrityFlag, IntegritySeverity, ItemState,
    JobId, JobState, OutputDisposition, OutputRecord, ProgressState,
};
use thiserror::Error;

use crate::{
    BackendCapabilities, BackendControl, BackendControlRequest, BackendEvent, BackendHealth,
    BackendItemDescriptor, BackendRunRequest, DownloadBackend,
};

#[derive(Debug, Clone, Default)]
pub struct ExternalDownloaderConfig {
    pub executable: PathBuf,
    pub working_directory: Option<PathBuf>,
    pub base_args: Vec<String>,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ExternalCommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum ExternalDownloaderError {
    #[error("external downloader executable is not configured")]
    MissingExecutable,
    #[error("external downloader executable was not found: {0}")]
    ExecutableNotFound(PathBuf),
    #[error("failed to spawn external downloader: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("external downloader did not expose a piped {0} stream")]
    MissingPipe(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalStreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug)]
enum ExternalProcessMessage {
    Line {
        stream: ExternalStreamKind,
        line: String,
    },
    ReadError {
        stream: ExternalStreamKind,
        message: String,
    },
    StreamClosed {
        stream: ExternalStreamKind,
    },
}

#[derive(Debug)]
struct ActiveExternalJob {
    request: BackendRunRequest,
    command: ExternalCommandSpec,
    child: Child,
    output_rx: Receiver<ExternalProcessMessage>,
    open_streams: u8,
    exit_status: Option<ExitStatus>,
    job_finished_emitted: bool,
    saw_item_failure: bool,
}

#[derive(Debug, Default)]
pub struct ExternalDownloader {
    config: ExternalDownloaderConfig,
    active_jobs: HashMap<JobId, ActiveExternalJob>,
}

#[derive(Debug, Deserialize)]
struct JsonEvent {
    event: String,
    #[serde(default)]
    track_id: Option<String>,
    #[serde(default, rename = "track")]
    track_label: Option<String>,
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    progress: Option<f64>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    attempt: Option<u64>,
    #[serde(default)]
    max_attempts: Option<u64>,
    #[serde(default)]
    delay_ms: Option<u64>,
    #[serde(default)]
    waited_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderDisposition {
    Parsed,
    Unparsed,
}

impl ExternalDownloader {
    pub fn new(config: ExternalDownloaderConfig) -> Self {
        Self {
            config,
            active_jobs: HashMap::new(),
        }
    }

    pub fn config(&self) -> &ExternalDownloaderConfig {
        &self.config
    }

    pub fn command_spec(
        &self,
        request: &BackendRunRequest,
    ) -> Result<ExternalCommandSpec, ExternalDownloaderError> {
        let executable = self.resolve_executable()?;

        let mut args = self.config.base_args.clone();

        if !request.options.format.trim().is_empty() {
            args.push("--format".to_string());
            args.push(request.options.format.clone());
        }

        if !request.options.destination.trim().is_empty() {
            args.push("--destination".to_string());
            args.push(request.options.destination.clone());
        }

        if request.options.max_parallel > 0 {
            args.push("--parallel".to_string());
            args.push(request.options.max_parallel.to_string());
        }

        args.push("--json-events".to_string());
        args.extend(request.options.extra_args.iter().cloned());
        args.extend(request.items.iter().map(|item| item.url.clone()));

        Ok(ExternalCommandSpec {
            program: executable,
            args,
            working_directory: self.config.working_directory.clone(),
            environment: self.config.environment.clone(),
        })
    }

    pub fn health_status(&self) -> BackendHealth {
        match self.resolve_executable() {
            Ok(path) => BackendHealth {
                ready: true,
                message: format!("Ready ({})", path.display()),
            },
            Err(error) => BackendHealth {
                ready: false,
                message: error.to_string(),
            },
        }
    }

    fn resolve_executable(&self) -> Result<PathBuf, ExternalDownloaderError> {
        let executable = self.config.executable.as_path();
        if executable.as_os_str().is_empty() {
            return resolve_default_bundled_executable()
                .ok_or(ExternalDownloaderError::MissingExecutable);
        }

        if is_explicit_path(executable) {
            return resolve_explicit_path(executable).ok_or_else(|| {
                ExternalDownloaderError::ExecutableNotFound(executable.to_path_buf())
            });
        }

        resolve_path_executable(executable)
            .or_else(|| resolve_bundled_named_executable(executable))
            .ok_or_else(|| ExternalDownloaderError::ExecutableNotFound(executable.to_path_buf()))
    }

    fn spawn_process(
        &self,
        command: &ExternalCommandSpec,
    ) -> Result<(Child, Receiver<ExternalProcessMessage>), ExternalDownloaderError> {
        let mut process = Command::new(&command.program);
        process.args(&command.args);
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        process.stdin(Stdio::null());

        if let Some(workdir) = &command.working_directory {
            process.current_dir(workdir);
        }

        for (key, value) in &command.environment {
            process.env(key, value);
        }

        let mut child = process.spawn().map_err(ExternalDownloaderError::Spawn)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(ExternalDownloaderError::MissingPipe("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(ExternalDownloaderError::MissingPipe("stderr"))?;

        let (tx, rx) = mpsc::channel();
        spawn_output_reader(stdout, ExternalStreamKind::Stdout, tx.clone());
        spawn_output_reader(stderr, ExternalStreamKind::Stderr, tx);

        Ok((child, rx))
    }

    fn drain_output(job_id: &JobId, job: &mut ActiveExternalJob, events: &mut Vec<BackendEvent>) {
        loop {
            match job.output_rx.try_recv() {
                Ok(message) => match message {
                    ExternalProcessMessage::Line { stream, line } => match stream {
                        ExternalStreamKind::Stdout => {
                            if Self::decode_stdout_line(job_id, job, &line, events)
                                == DecoderDisposition::Unparsed
                            {
                                events.push(BackendEvent::Log {
                                    job_id: Some(job_id.clone()),
                                    item_id: None,
                                    message: format!("[stdout] {}", line),
                                });
                            }
                        }
                        ExternalStreamKind::Stderr => {
                            events.push(BackendEvent::Log {
                                job_id: Some(job_id.clone()),
                                item_id: None,
                                message: format!("[stderr] {}", line),
                            });
                        }
                    },
                    ExternalProcessMessage::ReadError { stream, message } => {
                        events.push(BackendEvent::Log {
                            job_id: Some(job_id.clone()),
                            item_id: None,
                            message: format!(
                                "[{}] adapter stream read error: {}",
                                stream_label(stream),
                                message
                            ),
                        });
                    }
                    ExternalProcessMessage::StreamClosed { stream } => {
                        if job.open_streams > 0 {
                            job.open_streams -= 1;
                        }

                        events.push(BackendEvent::Log {
                            job_id: Some(job_id.clone()),
                            item_id: None,
                            message: format!("[{}] stream closed", stream_label(stream)),
                        });
                    }
                },
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn decode_stdout_line(
        job_id: &JobId,
        job: &mut ActiveExternalJob,
        line: &str,
        events: &mut Vec<BackendEvent>,
    ) -> DecoderDisposition {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            return DecoderDisposition::Unparsed;
        }

        let event = match serde_json::from_str::<JsonEvent>(trimmed) {
            Ok(event) => event,
            Err(error) => {
                events.push(BackendEvent::Log {
                    job_id: Some(job_id.clone()),
                    item_id: None,
                    message: format!("Failed to parse json event: {}", error),
                });
                return DecoderDisposition::Unparsed;
            }
        };

        Self::apply_json_event(job_id, job, event, events);
        DecoderDisposition::Parsed
    }

    fn apply_json_event(
        job_id: &JobId,
        job: &mut ActiveExternalJob,
        event: JsonEvent,
        events: &mut Vec<BackendEvent>,
    ) {
        let item = Self::resolve_item(job, event.track_id.as_deref(), event.track_label.as_deref());
        let item_id = item.map(|item| item.item_id.clone());

        match event.event.as_str() {
            "track_start" => {
                if let Some(item_id) = item_id {
                    events.push(BackendEvent::ItemStarted {
                        job_id: job_id.clone(),
                        item_id,
                        label: event.track_label,
                        metadata: None,
                    });
                } else {
                    events.push(BackendEvent::Log {
                        job_id: Some(job_id.clone()),
                        item_id: None,
                        message: format!(
                            "Unmatched track_start event for {:?}",
                            event.track_label.as_deref().or(event.track_id.as_deref())
                        ),
                    });
                }
            }
            "stage" => {
                if let Some(item_id) = item_id {
                    let detail = build_stage_detail(
                        event.stage.as_deref(),
                        event.status.as_deref(),
                        event.track_label.as_deref(),
                    );
                    let progress = progress_from_event(event.progress.unwrap_or(0.0), detail);
                    events.push(BackendEvent::ItemProgress {
                        job_id: job_id.clone(),
                        item_id,
                        progress,
                    });
                }
            }
            "track_complete" => {
                if let Some(item_id) = item_id {
                    if let Some(path) = event.path.clone() {
                        events.push(BackendEvent::ItemOutput {
                            job_id: job_id.clone(),
                            item_id: item_id.clone(),
                            output: OutputRecord {
                                disposition: OutputDisposition::Written,
                                final_path: path.clone(),
                                artist: String::new(),
                                album: String::new(),
                                title: event.track_label.clone().unwrap_or_default(),
                                size_bytes: 0,
                                format: path
                                    .rsplit('.')
                                    .next()
                                    .map(ToOwned::to_owned)
                                    .unwrap_or_default(),
                                details: Some(
                                    "Reported by external downloader json event".to_string(),
                                ),
                            },
                        });
                    }

                    events.push(BackendEvent::ItemFinished {
                        job_id: job_id.clone(),
                        item_id,
                        state: ItemState::Completed,
                        failure_reason: None,
                    });
                }
            }
            "track_skipped" => {
                if let Some(item_id) = item_id {
                    events.push(BackendEvent::ItemFlag {
                        job_id: job_id.clone(),
                        item_id: item_id.clone(),
                        flag: IntegrityFlag {
                            severity: IntegritySeverity::Info,
                            kind: "skip".to_string(),
                            message: "External downloader skipped existing output".to_string(),
                            details: event.track_label.clone(),
                        },
                    });
                    events.push(BackendEvent::ItemFinished {
                        job_id: job_id.clone(),
                        item_id,
                        state: ItemState::Completed,
                        failure_reason: None,
                    });
                }
            }
            "track_failed" => {
                if let Some(item_id) = item_id {
                    job.saw_item_failure = true;
                    events.push(BackendEvent::ItemFinished {
                        job_id: job_id.clone(),
                        item_id,
                        state: ItemState::Failed,
                        failure_reason: Some(classify_failure_reason(event.reason.as_deref())),
                    });
                }
            }
            "retry" => {
                events.push(BackendEvent::Log {
                    job_id: Some(job_id.clone()),
                    item_id,
                    message: format!(
                        "Retrying {} stage {:?} attempt {}/{}",
                        event.track_label.as_deref().unwrap_or("item"),
                        event.stage.as_deref().unwrap_or("unknown"),
                        event.attempt.unwrap_or(0),
                        event.max_attempts.unwrap_or(0)
                    ),
                });
            }
            "rate_limit_backoff" => {
                let state = BackoffState {
                    active: true,
                    reason: event
                        .reason
                        .clone()
                        .unwrap_or_else(|| "External downloader backoff".to_string()),
                    delay_ms: event.delay_ms.unwrap_or(0),
                    remaining_ms: event.delay_ms.unwrap_or(0),
                };
                events.push(BackendEvent::BackoffStarted {
                    job_id: Some(job_id.clone()),
                    state,
                });
            }
            "rate_limit_wait" => {
                let waited_ms = event.waited_ms.unwrap_or(0);
                let remaining_ms = 0;
                events.push(BackendEvent::BackoffTick {
                    job_id: Some(job_id.clone()),
                    state: BackoffState {
                        active: false,
                        reason: "External downloader wait finished".to_string(),
                        delay_ms: waited_ms,
                        remaining_ms,
                    },
                });
            }
            other => {
                events.push(BackendEvent::Log {
                    job_id: Some(job_id.clone()),
                    item_id,
                    message: format!("Unhandled external json event `{other}`"),
                });
            }
        }
    }

    fn resolve_item<'a>(
        job: &'a ActiveExternalJob,
        track_id: Option<&str>,
        track_label: Option<&str>,
    ) -> Option<&'a BackendItemDescriptor> {
        if job.request.items.len() == 1 {
            return job.request.items.first();
        }

        if let Some(track_id) = track_id {
            if let Some(item) = job
                .request
                .items
                .iter()
                .find(|item| spotify_identity(&item.url).as_deref() == Some(track_id))
            {
                return Some(item);
            }
        }

        if let Some(track_label) = track_label {
            if let Some(item) = job
                .request
                .items
                .iter()
                .find(|item| item.label == track_label)
            {
                return Some(item);
            }
        }

        None
    }

    fn decode_exit(
        job_id: &JobId,
        job: &ActiveExternalJob,
        status: ExitStatus,
    ) -> Vec<BackendEvent> {
        let mut events = vec![BackendEvent::Log {
            job_id: Some(job_id.clone()),
            item_id: None,
            message: format!(
                "External downloader exited with status {:?} for {}",
                status.code(),
                render_command_preview(&job.command)
            ),
        }];

        if status.success() {
            if !job.job_finished_emitted {
                events.push(BackendEvent::JobFinished {
                    job_id: job_id.clone(),
                    state: if job.saw_item_failure {
                        JobState::Failed
                    } else {
                        JobState::Completed
                    },
                    failure_reason: if job.saw_item_failure {
                        Some(FailureReason {
                            kind: FailureReasonKind::Unknown,
                            code: Some("ITEM_FAILURES_REPORTED".to_string()),
                            message: "External downloader reported one or more item failures"
                                .to_string(),
                            details: Some(render_command_preview(&job.command)),
                        })
                    } else {
                        None
                    },
                });
            }
        } else {
            events.push(BackendEvent::JobFinished {
                job_id: job_id.clone(),
                state: JobState::Failed,
                failure_reason: Some(FailureReason {
                    kind: FailureReasonKind::Unknown,
                    code: status.code().map(|code| format!("EXIT_{code}")),
                    message: "External downloader exited unsuccessfully".to_string(),
                    details: Some(render_command_preview(&job.command)),
                }),
            });
        }

        events
    }
}

impl DownloadBackend for ExternalDownloader {
    fn name(&self) -> &'static str {
        "external-downloader"
    }

    fn health(&self) -> BackendHealth {
        self.health_status()
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    fn validate_run_request(&self, request: &BackendRunRequest) -> Result<(), String> {
        if request.items.is_empty() {
            return Err("external downloader request did not include any items".to_string());
        }

        self.command_spec(request)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn start(&mut self, request: BackendRunRequest) -> Vec<BackendEvent> {
        match self.command_spec(&request) {
            Ok(command) => match self.spawn_process(&command) {
                Ok((child, output_rx)) => {
                    let job_id = request.job_id.clone();
                    let preview = render_command_preview(&command);
                    self.active_jobs.insert(
                        job_id.clone(),
                        ActiveExternalJob {
                            request,
                            command,
                            child,
                            output_rx,
                            open_streams: 2,
                            exit_status: None,
                            job_finished_emitted: false,
                            saw_item_failure: false,
                        },
                    );

                    vec![
                        BackendEvent::Log {
                            job_id: Some(job_id.clone()),
                            item_id: None,
                            message: format!("Launching external downloader: {preview}"),
                        },
                        BackendEvent::JobStarted { job_id },
                    ]
                }
                Err(error) => vec![
                    BackendEvent::Log {
                        job_id: Some(request.job_id.clone()),
                        item_id: None,
                        message: format!("Failed to launch external downloader: {error}"),
                    },
                    BackendEvent::JobFinished {
                        job_id: request.job_id,
                        state: JobState::Failed,
                        failure_reason: Some(FailureReason {
                            kind: FailureReasonKind::Io,
                            code: Some("BACKEND_SPAWN_FAILED".to_string()),
                            message: error.to_string(),
                            details: None,
                        }),
                    },
                ],
            },
            Err(error) => vec![
                BackendEvent::Log {
                    job_id: Some(request.job_id.clone()),
                    item_id: None,
                    message: format!("External downloader configuration error: {error}"),
                },
                BackendEvent::JobFinished {
                    job_id: request.job_id,
                    state: JobState::Failed,
                    failure_reason: Some(FailureReason {
                        kind: FailureReasonKind::Io,
                        code: Some("BACKEND_CONFIGURATION".to_string()),
                        message: error.to_string(),
                        details: None,
                    }),
                },
            ],
        }
    }

    fn control(&mut self, request: BackendControlRequest) -> Vec<BackendEvent> {
        match request.control {
            BackendControl::Pause => {
                if let Some(job) = self.active_jobs.get(&request.job_id) {
                    return vec![BackendEvent::Log {
                        job_id: Some(request.job_id),
                        item_id: None,
                        message: format!(
                            "Pause is not implemented for the external process backend yet ({})",
                            job.request.label
                        ),
                    }];
                }

                Vec::new()
            }
            BackendControl::Resume => {
                if let Some(job) = self.active_jobs.get(&request.job_id) {
                    return vec![BackendEvent::Log {
                        job_id: Some(request.job_id),
                        item_id: None,
                        message: format!(
                            "Resume is not implemented for the external process backend yet ({})",
                            job.request.label
                        ),
                    }];
                }

                Vec::new()
            }
            BackendControl::Cancel => {
                let Some(mut job) = self.active_jobs.remove(&request.job_id) else {
                    return Vec::new();
                };

                let _ = job.child.kill();
                let _ = job.child.wait();

                let mut events = Vec::new();
                Self::drain_output(&request.job_id, &mut job, &mut events);
                events.push(BackendEvent::Log {
                    job_id: Some(request.job_id.clone()),
                    item_id: None,
                    message: "Killed external downloader process".to_string(),
                });
                events.push(BackendEvent::JobFinished {
                    job_id: request.job_id,
                    state: JobState::Cancelled,
                    failure_reason: Some(FailureReason {
                        kind: FailureReasonKind::Cancelled,
                        code: Some("USER_CANCELLED".to_string()),
                        message: "Cancelled by user".to_string(),
                        details: Some(render_command_preview(&job.command)),
                    }),
                });
                events
            }
        }
    }

    fn tick(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        let mut completed = Vec::new();

        for (job_id, job) in &mut self.active_jobs {
            Self::drain_output(job_id, job, &mut events);

            if job.exit_status.is_none() {
                match job.child.try_wait() {
                    Ok(Some(status)) => {
                        job.exit_status = Some(status);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        events.push(BackendEvent::Log {
                            job_id: Some(job_id.clone()),
                            item_id: None,
                            message: format!("Failed to poll external downloader: {error}"),
                        });
                        completed.push(job_id.clone());
                        events.push(BackendEvent::JobFinished {
                            job_id: job_id.clone(),
                            state: JobState::Failed,
                            failure_reason: Some(FailureReason {
                                kind: FailureReasonKind::Io,
                                code: Some("BACKEND_WAIT_FAILED".to_string()),
                                message: error.to_string(),
                                details: Some(render_command_preview(&job.command)),
                            }),
                        });
                        continue;
                    }
                }
            }

            Self::drain_output(job_id, job, &mut events);

            if let Some(status) = job.exit_status {
                if status.success() && contains_job_finished(&events, job_id) {
                    job.job_finished_emitted = true;
                }

                if job.open_streams == 0 {
                    events.extend(Self::decode_exit(job_id, job, status));
                    completed.push(job_id.clone());
                }
            }
        }

        for job_id in completed {
            self.active_jobs.remove(&job_id);
        }

        events
    }
}

fn spawn_output_reader<R>(reader: R, stream: ExternalStreamKind, tx: Sender<ExternalProcessMessage>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines() {
            match line {
                Ok(line) => {
                    let _ = tx.send(ExternalProcessMessage::Line { stream, line });
                }
                Err(error) => {
                    let _ = tx.send(ExternalProcessMessage::ReadError {
                        stream,
                        message: error.to_string(),
                    });
                    let _ = tx.send(ExternalProcessMessage::StreamClosed { stream });
                    return;
                }
            }
        }

        let _ = tx.send(ExternalProcessMessage::StreamClosed { stream });
    });
}

fn contains_job_finished(events: &[BackendEvent], job_id: &JobId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            BackendEvent::JobFinished { job_id: event_job_id, .. } if event_job_id == job_id
        )
    })
}

fn is_explicit_path(path: &Path) -> bool {
    path.is_absolute() || path.parent().is_some()
}

fn resolve_explicit_path(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }

    executable_candidates(path.file_name()?)
        .into_iter()
        .find_map(|candidate| {
            let mut full = path.to_path_buf();
            full.set_file_name(candidate);
            full.is_file().then_some(full)
        })
}

fn resolve_path_executable(executable: &Path) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    let file_name = executable.file_name()?;

    env::split_paths(&path_var).find_map(|directory| {
        executable_candidates(file_name)
            .into_iter()
            .map(|candidate| directory.join(candidate))
            .find(|full| full.is_file())
    })
}

fn resolve_default_bundled_executable() -> Option<PathBuf> {
    resolve_bundled_named_executable(Path::new("spotify-dl"))
}

fn resolve_bundled_named_executable(executable: &Path) -> Option<PathBuf> {
    let file_name = executable.file_name()?;
    let current_exe = env::current_exe().ok()?;
    let base_dir = current_exe.parent()?;

    resolve_bundled_named_executable_from(base_dir, file_name)
}

fn resolve_bundled_named_executable_from(
    base_dir: &Path,
    file_name: &std::ffi::OsStr,
) -> Option<PathBuf> {
    [PathBuf::new(), PathBuf::from("bin"), PathBuf::from("tools")]
        .into_iter()
        .find_map(|relative| {
            let directory = base_dir.join(relative);
            executable_candidates(file_name)
                .into_iter()
                .map(|candidate| directory.join(candidate))
                .find(|full| full.is_file())
        })
}

fn executable_candidates(file_name: &std::ffi::OsStr) -> Vec<OsString> {
    let mut candidates = vec![file_name.to_os_string()];

    #[cfg(windows)]
    if Path::new(file_name).extension().is_none() {
        let pathext =
            env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
        for ext in pathext
            .to_string_lossy()
            .split(';')
            .map(str::trim)
            .filter(|ext| !ext.is_empty())
        {
            let mut candidate = file_name.to_os_string();
            candidate.push(ext);
            candidates.push(candidate);
        }
    }

    candidates
}

fn build_stage_detail(stage: Option<&str>, status: Option<&str>, label: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(stage) = stage {
        parts.push(stage.to_string());
    }
    if let Some(status) = status {
        parts.push(status.to_string());
    }
    if let Some(label) = label {
        parts.push(label.to_string());
    }
    if parts.is_empty() {
        "processing".to_string()
    } else {
        parts.join(" | ")
    }
}

fn progress_from_event(progress: f64, detail: String) -> ProgressState {
    let clamped = progress.clamp(0.0, 100.0);
    ProgressState::from_steps(clamped.round() as u32, 100, detail)
}

fn classify_failure_reason(reason: Option<&str>) -> FailureReason {
    let message = reason.unwrap_or("External downloader reported a failure");
    let lower = message.to_ascii_lowercase();
    let kind = if lower.contains("cancel") {
        FailureReasonKind::Cancelled
    } else if lower.contains("rate") || lower.contains("throttle") {
        FailureReasonKind::RateLimit
    } else if lower.contains("network") || lower.contains("stream") || lower.contains("timeout") {
        FailureReasonKind::Network
    } else if lower.contains("metadata") {
        FailureReasonKind::Metadata
    } else if lower.contains("auth") || lower.contains("login") || lower.contains("premium") {
        FailureReasonKind::Auth
    } else if lower.contains("file") || lower.contains("write") || lower.contains("path") {
        FailureReasonKind::Io
    } else {
        FailureReasonKind::Unknown
    };

    FailureReason {
        kind,
        code: None,
        message: message.to_string(),
        details: None,
    }
}

fn spotify_identity(url_or_uri: &str) -> Option<String> {
    let trimmed = url_or_uri.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("spotify:") {
        return rest.rsplit(':').next().map(ToOwned::to_owned);
    }

    let path = trimmed.split('?').next().unwrap_or(trimmed);
    path.rsplit('/').next().map(ToOwned::to_owned)
}

fn render_command_preview(command: &ExternalCommandSpec) -> String {
    let program = display_path(&command.program);
    let args = if command.args.is_empty() {
        String::new()
    } else {
        format!(" {}", command.args.join(" "))
    };

    format!("{program}{args}")
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn stream_label(stream: ExternalStreamKind) -> &'static str {
    match stream {
        ExternalStreamKind::Stdout => "stdout",
        ExternalStreamKind::Stderr => "stderr",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::BackendItemDescriptor;
    use spotifydl_protocol::{ItemId, ItemMetadata, JobId, JobOptions};

    fn request_for_test() -> BackendRunRequest {
        BackendRunRequest {
            job_id: JobId("job-1".to_string()),
            label: "Example".to_string(),
            source_url: "https://open.spotify.com/playlist/demo".to_string(),
            options: JobOptions {
                destination: "C:/music".to_string(),
                format: "flac".to_string(),
                max_parallel: 3,
                extra_args: vec!["--force".to_string()],
            },
            items: vec![BackendItemDescriptor {
                item_id: ItemId("item-1".to_string()),
                position: 0,
                label: "Track 1".to_string(),
                url: "https://open.spotify.com/track/abc123".to_string(),
                metadata: ItemMetadata::default(),
            }],
        }
    }

    #[test]
    fn builds_command_spec_from_request() {
        let executable = std::env::current_exe().expect("current test executable");
        let downloader = ExternalDownloader::new(ExternalDownloaderConfig {
            executable: executable.clone(),
            working_directory: Some(PathBuf::from("C:/work")),
            base_args: vec![],
            environment: vec![("SPOTIFYDL_ENV".to_string(), "1".to_string())],
        });

        let spec = downloader
            .command_spec(&request_for_test())
            .expect("command spec should build");

        assert_eq!(spec.program, executable);
        assert_eq!(spec.working_directory, Some(PathBuf::from("C:/work")));
        assert_eq!(
            spec.args,
            vec![
                "--format".to_string(),
                "flac".to_string(),
                "--destination".to_string(),
                "C:/music".to_string(),
                "--parallel".to_string(),
                "3".to_string(),
                "--json-events".to_string(),
                "--force".to_string(),
                "https://open.spotify.com/track/abc123".to_string(),
            ]
        );
        assert_eq!(
            spec.environment,
            vec![("SPOTIFYDL_ENV".to_string(), "1".to_string())]
        );
    }

    #[test]
    fn decodes_track_complete_json_event() {
        let mut job = ActiveExternalJob {
            request: request_for_test(),
            command: ExternalCommandSpec {
                program: PathBuf::from("spotify-dl"),
                args: Vec::new(),
                working_directory: None,
                environment: Vec::new(),
            },
            child: Command::new("cmd")
                .args(["/C", "echo", "ok"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn test child"),
            output_rx: mpsc::channel().1,
            open_streams: 0,
            exit_status: None,
            job_finished_emitted: false,
            saw_item_failure: false,
        };

        let mut events = Vec::new();
        let disposition = ExternalDownloader::decode_stdout_line(
            &JobId("job-1".to_string()),
            &mut job,
            r#"{"event":"track_complete","track_id":"abc123","track":"Artist - Song","path":"C:/music/song.flac"}"#,
            &mut events,
        );

        assert_eq!(disposition, DecoderDisposition::Parsed);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, BackendEvent::ItemOutput { .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            BackendEvent::ItemFinished {
                state: ItemState::Completed,
                ..
            }
        )));

        let _ = job.child.kill();
        let _ = job.child.wait();
    }

    #[test]
    fn resolves_spotify_identities_from_urls_and_uris() {
        assert_eq!(
            spotify_identity("https://open.spotify.com/track/abc123?si=xyz").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            spotify_identity("spotify:track:def456").as_deref(),
            Some("def456")
        );
    }

    #[test]
    fn reports_unready_health_when_executable_path_is_missing() {
        let downloader = ExternalDownloader::new(ExternalDownloaderConfig {
            executable: PathBuf::from("C:/definitely-missing/spotify-dl.exe"),
            working_directory: None,
            base_args: Vec::new(),
            environment: Vec::new(),
        });

        let health = downloader.health_status();
        assert!(!health.ready);
        assert!(health.message.contains("not found"));
    }

    #[test]
    fn resolves_bundled_executable_from_bin_directory() {
        let temp_root = temp_test_dir();
        let bin_dir = temp_root.join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        let bundled = bin_dir.join(if cfg!(windows) {
            "spotify-dl.exe"
        } else {
            "spotify-dl"
        });
        fs::write(&bundled, b"test").expect("write fake executable");

        let resolved =
            resolve_bundled_named_executable_from(&temp_root, std::ffi::OsStr::new("spotify-dl"))
                .expect("bundled executable should resolve");
        assert_eq!(
            resolved.to_string_lossy().to_ascii_lowercase(),
            bundled.to_string_lossy().to_ascii_lowercase()
        );

        let _ = fs::remove_file(&bundled);
        let _ = fs::remove_dir_all(&temp_root);
    }

    fn temp_test_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should advance")
            .as_nanos();
        std::env::temp_dir().join(format!("spotifydl-core-test-{unique}"))
    }
}
