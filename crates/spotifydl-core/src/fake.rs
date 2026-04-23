use std::collections::HashMap;

use spotifydl_protocol::{
    BackoffState, FailureReason, FailureReasonKind, IntegrityFlag, IntegritySeverity, ItemMetadata,
    ItemState, JobId, JobState, OutputDisposition, OutputRecord, ProgressState,
};

use crate::{
    BackendCapabilities, BackendControl, BackendControlRequest, BackendEvent,
    BackendItemDescriptor, BackendRunRequest, DownloadBackend,
};

#[derive(Debug, Clone)]
struct ActiveFakeJob {
    request: BackendRunRequest,
    current_item_index: usize,
    current_step: u32,
    paused: bool,
    active_item_started: bool,
    backoff_ticks_remaining: u8,
    backoff_injected: bool,
    saw_failure: bool,
}

#[derive(Default)]
pub struct FakeDownloader {
    active_jobs: HashMap<JobId, ActiveFakeJob>,
}

impl DownloadBackend for FakeDownloader {
    fn name(&self) -> &'static str {
        "fake-downloader"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_immediate_pause_resume: true,
        }
    }

    fn start(&mut self, request: BackendRunRequest) -> Vec<BackendEvent> {
        let job_id = request.job_id.clone();
        let label = request.label.clone();
        let source_url = request.source_url.clone();

        self.active_jobs.insert(
            job_id.clone(),
            ActiveFakeJob {
                request,
                current_item_index: 0,
                current_step: 0,
                paused: false,
                active_item_started: false,
                backoff_ticks_remaining: 0,
                backoff_injected: false,
                saw_failure: false,
            },
        );

        vec![
            BackendEvent::Log {
                job_id: Some(job_id.clone()),
                item_id: None,
                message: format!("Fake downloader accepted {label} ({source_url})"),
            },
            BackendEvent::JobStarted { job_id },
        ]
    }

    fn control(&mut self, request: BackendControlRequest) -> Vec<BackendEvent> {
        match request.control {
            BackendControl::Pause => self.pause(&request.job_id),
            BackendControl::Resume => self.resume(&request.job_id),
            BackendControl::Cancel => self.cancel(&request.job_id),
        }
    }

    fn tick(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        let mut finished_jobs = Vec::new();

        for (job_id, job) in &mut self.active_jobs {
            if job.paused {
                continue;
            }

            if job.backoff_ticks_remaining > 0 {
                job.backoff_ticks_remaining -= 1;
                let remaining_ms = u64::from(job.backoff_ticks_remaining) * 750;
                events.push(BackendEvent::BackoffTick {
                    job_id: Some(job_id.clone()),
                    state: BackoffState {
                        active: job.backoff_ticks_remaining > 0,
                        reason: "Simulated rate limit".to_string(),
                        delay_ms: 1_500,
                        remaining_ms,
                    },
                });
                continue;
            }

            let Some(item) = job.request.items.get(job.current_item_index).cloned() else {
                events.push(BackendEvent::JobFinished {
                    job_id: job_id.clone(),
                    state: if job.saw_failure {
                        JobState::Failed
                    } else {
                        JobState::Completed
                    },
                    failure_reason: None,
                });
                finished_jobs.push(job_id.clone());
                continue;
            };

            if !job.active_item_started {
                job.active_item_started = true;
                events.push(BackendEvent::ItemStarted {
                    job_id: job_id.clone(),
                    item_id: item.item_id.clone(),
                    label: Some(item.label.clone()),
                    metadata: Some(fake_metadata(&item)),
                });
            }

            job.current_step = (job.current_step + 25).min(100);
            events.push(BackendEvent::ItemProgress {
                job_id: job_id.clone(),
                item_id: item.item_id.clone(),
                progress: ProgressState::from_steps(
                    job.current_step,
                    100,
                    format!("Fake downloader processing {}", item.label),
                ),
            });

            if should_backoff(&item) && !job.backoff_injected && job.current_step >= 50 {
                job.backoff_injected = true;
                job.backoff_ticks_remaining = 2;
                events.push(BackendEvent::BackoffStarted {
                    job_id: Some(job_id.clone()),
                    state: BackoffState {
                        active: true,
                        reason: "Simulated rate limit".to_string(),
                        delay_ms: 1_500,
                        remaining_ms: 1_500,
                    },
                });
                continue;
            }

            if job.current_step < 100 {
                continue;
            }

            if should_fail(&item) {
                job.saw_failure = true;
                events.push(BackendEvent::ItemFinished {
                    job_id: job_id.clone(),
                    item_id: item.item_id.clone(),
                    state: ItemState::Failed,
                    failure_reason: Some(FailureReason {
                        kind: FailureReasonKind::Network,
                        code: Some("FAKE_NETWORK".to_string()),
                        message: "Simulated network failure".to_string(),
                        details: Some("Retry against the real adapter once integrated".to_string()),
                    }),
                });
            } else {
                events.push(BackendEvent::ItemOutput {
                    job_id: job_id.clone(),
                    item_id: item.item_id.clone(),
                    output: fake_output(&item),
                });

                if should_flag(&item) {
                    events.push(BackendEvent::ItemFlag {
                        job_id: job_id.clone(),
                        item_id: item.item_id.clone(),
                        flag: IntegrityFlag {
                            severity: IntegritySeverity::Warning,
                            kind: "integrity".to_string(),
                            message: "Simulated duration mismatch warning".to_string(),
                            details: Some("Verifier reported a small drift".to_string()),
                        },
                    });
                }

                events.push(BackendEvent::ItemFinished {
                    job_id: job_id.clone(),
                    item_id: item.item_id.clone(),
                    state: ItemState::Completed,
                    failure_reason: None,
                });
            }

            job.current_item_index += 1;
            job.current_step = 0;
            job.active_item_started = false;
            job.backoff_injected = false;
        }

        for job_id in finished_jobs {
            self.active_jobs.remove(&job_id);
        }

        events
    }
}

impl FakeDownloader {
    fn pause(&mut self, job_id: &JobId) -> Vec<BackendEvent> {
        if let Some(job) = self.active_jobs.get_mut(job_id) {
            job.paused = true;
            return vec![BackendEvent::Log {
                job_id: Some(job_id.clone()),
                item_id: None,
                message: format!("Fake downloader paused {}", job.request.label),
            }];
        }

        Vec::new()
    }

    fn resume(&mut self, job_id: &JobId) -> Vec<BackendEvent> {
        if let Some(job) = self.active_jobs.get_mut(job_id) {
            job.paused = false;
            return vec![BackendEvent::Log {
                job_id: Some(job_id.clone()),
                item_id: None,
                message: format!("Fake downloader resumed {}", job.request.label),
            }];
        }

        Vec::new()
    }

    fn cancel(&mut self, job_id: &JobId) -> Vec<BackendEvent> {
        let Some(job) = self.active_jobs.remove(job_id) else {
            return Vec::new();
        };

        let mut events = vec![BackendEvent::Log {
            job_id: Some(job_id.clone()),
            item_id: None,
            message: format!("Fake downloader cancelled {}", job.request.label),
        }];

        if let Some(item) = job.request.items.get(job.current_item_index) {
            events.push(BackendEvent::ItemFinished {
                job_id: job_id.clone(),
                item_id: item.item_id.clone(),
                state: ItemState::Cancelled,
                failure_reason: Some(FailureReason {
                    kind: FailureReasonKind::Cancelled,
                    code: Some("USER_CANCELLED".to_string()),
                    message: "Cancelled by user".to_string(),
                    details: None,
                }),
            });
        }

        events.push(BackendEvent::JobFinished {
            job_id: job_id.clone(),
            state: JobState::Cancelled,
            failure_reason: Some(FailureReason {
                kind: FailureReasonKind::Cancelled,
                code: Some("USER_CANCELLED".to_string()),
                message: "Cancelled by user".to_string(),
                details: None,
            }),
        });

        events
    }
}

fn fake_metadata(item: &BackendItemDescriptor) -> ItemMetadata {
    ItemMetadata {
        artist: "Fake Artist".to_string(),
        album: format!("Mock Album {}", item.position + 1),
        title: item.label.clone(),
        album_artist: "Fake Artist".to_string(),
    }
}

fn fake_output(item: &BackendItemDescriptor) -> OutputRecord {
    OutputRecord {
        disposition: OutputDisposition::Written,
        final_path: format!("mock-output/{}.flac", sanitize_label(&item.label)),
        artist: "Fake Artist".to_string(),
        album: format!("Mock Album {}", item.position + 1),
        title: item.label.clone(),
        size_bytes: 5_242_880,
        format: "flac".to_string(),
        details: Some("Simulated adapter output".to_string()),
    }
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn should_backoff(item: &BackendItemDescriptor) -> bool {
    item.url.contains("rate") || item.position == 0
}

fn should_fail(item: &BackendItemDescriptor) -> bool {
    item.url.contains("fail")
}

fn should_flag(item: &BackendItemDescriptor) -> bool {
    item.url.contains("warn") || item.position % 2 == 1
}
