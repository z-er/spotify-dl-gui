use std::{path::PathBuf, time::Duration};

use chrono::{Local, TimeZone};
use directories::ProjectDirs;
use iced::{
    Element, Length, Subscription, Task, Theme, application, time,
    widget::{button, column, container, row, scrollable, text, text_input},
};
use spotifydl_protocol::{
    AppSnapshot, BackendKind, DownloadItem, HistoryEntry, JobId, JobRecord, QueueStatus,
    ServiceCommand,
};
use spotifydl_service::SpotifydlService;

pub fn main() -> iced::Result {
    application("spotifydl Rust Scaffold", update, view)
        .subscription(subscription)
        .theme(|_| Theme::TokyoNight)
        .run_with(initialize)
}

struct SpotifydlGuiApp {
    service: SpotifydlService,
    snapshot: AppSnapshot,
    url_input: String,
    selected_backend: BackendKind,
    external_backend_input: String,
    default_destination_input: String,
    default_format_input: String,
    max_parallel_input: String,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
enum Message {
    UrlInputChanged(String),
    AddUrls,
    RunOrPause,
    CancelJob(JobId),
    RequeueHistoryJob(JobId),
    RemoveHistoryJob(JobId),
    ClearHistory,
    SelectBackend(BackendKind),
    ExternalBackendInputChanged(String),
    DefaultDestinationInputChanged(String),
    DefaultFormatInputChanged(String),
    MaxParallelInputChanged(String),
    SaveBackendSettings,
    Tick,
}

fn initialize() -> (SpotifydlGuiApp, Task<Message>) {
    let database_path = default_database_path();
    let service = SpotifydlService::open(database_path).expect("failed to start service");
    let snapshot = service.snapshot().clone();
    let selected_backend = snapshot.settings.preferred_backend;
    let external_backend_input = snapshot.settings.external_backend_executable.clone();
    let default_destination_input = snapshot.settings.default_destination.clone();
    let default_format_input = snapshot.settings.default_format.clone();
    let max_parallel_input = snapshot.settings.max_parallel.to_string();

    (
        SpotifydlGuiApp {
            service,
            snapshot,
            url_input: String::new(),
            selected_backend,
            external_backend_input,
            default_destination_input,
            default_format_input,
            max_parallel_input,
            last_error: None,
        },
        Task::none(),
    )
}

fn update(app: &mut SpotifydlGuiApp, message: Message) -> Task<Message> {
    match message {
        Message::UrlInputChanged(value) => {
            app.url_input = value;
        }
        Message::AddUrls => {
            let urls = app
                .url_input
                .split_whitespace()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();

            if !urls.is_empty() {
                dispatch(
                    app,
                    ServiceCommand::EnqueueUrls {
                        urls,
                        source: None,
                        options: None,
                    },
                );
                app.url_input.clear();
            }
        }
        Message::RunOrPause => {
            let command = if app.snapshot.queue.status == QueueStatus::Running {
                ServiceCommand::PauseQueue
            } else {
                ServiceCommand::StartQueue
            };
            dispatch(app, command);
        }
        Message::CancelJob(job_id) => {
            dispatch(app, ServiceCommand::CancelJob { job_id });
        }
        Message::RequeueHistoryJob(job_id) => {
            dispatch(app, ServiceCommand::RequeueHistoryJob { job_id });
        }
        Message::RemoveHistoryJob(job_id) => {
            dispatch(
                app,
                ServiceCommand::RemoveJob {
                    job_id,
                    from_history: true,
                },
            );
        }
        Message::ClearHistory => {
            dispatch(app, ServiceCommand::ClearHistory);
        }
        Message::SelectBackend(backend) => {
            app.selected_backend = backend;
        }
        Message::ExternalBackendInputChanged(value) => {
            app.external_backend_input = value;
        }
        Message::DefaultDestinationInputChanged(value) => {
            app.default_destination_input = value;
        }
        Message::DefaultFormatInputChanged(value) => {
            app.default_format_input = value;
        }
        Message::MaxParallelInputChanged(value) => {
            app.max_parallel_input = value;
        }
        Message::SaveBackendSettings => {
            let max_parallel = match app.max_parallel_input.trim() {
                "" => 0,
                value => match value.parse::<u16>() {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        app.last_error =
                            Some("Max parallel must be a whole number between 0 and 65535".into());
                        return Task::none();
                    }
                },
            };
            let mut settings = app.snapshot.settings.clone();
            settings.preferred_backend = app.selected_backend;
            settings.external_backend_executable = app.external_backend_input.trim().to_string();
            settings.default_destination = app.default_destination_input.trim().to_string();
            settings.default_format = app.default_format_input.trim().to_string();
            settings.max_parallel = max_parallel;
            if dispatch(app, ServiceCommand::UpdateSettings { settings }) {
                sync_settings_inputs(app);
            }
        }
        Message::Tick => {
            dispatch(app, ServiceCommand::Tick);
        }
    }

    Task::none()
}

fn dispatch(app: &mut SpotifydlGuiApp, command: ServiceCommand) -> bool {
    match app.service.dispatch(command) {
        Ok(_) => {
            app.snapshot = app.service.snapshot().clone();
            app.last_error = None;
            true
        }
        Err(error) => {
            app.last_error = Some(error.to_string());
            false
        }
    }
}

fn subscription(app: &SpotifydlGuiApp) -> Subscription<Message> {
    if app.snapshot.queue.active_job_id.is_some()
        || matches!(
            app.snapshot.queue.status,
            QueueStatus::Running | QueueStatus::Backoff
        )
    {
        time::every(Duration::from_millis(250)).map(|_| Message::Tick)
    } else {
        Subscription::none()
    }
}

fn view(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let queue_panel = column![
        text("Queue").size(28),
        queue_controls(app),
        queue_list(&app.snapshot.queue.jobs),
    ]
    .spacing(16)
    .width(Length::FillPortion(3));

    let side_panel = column![settings_panel(app), history_panel(app), logs_panel(app)]
        .spacing(16)
        .width(Length::FillPortion(2));

    let status_line = if let Some(error) = &app.last_error {
        text(format!("Error: {error}"))
    } else {
        text(format!(
            "Database: {} | Backend: {} | {}",
            app.service.database_path().display(),
            app.snapshot.service_health.backend_name,
            format_backend_status(&app.snapshot)
        ))
    };

    let content = column![
        text("spotifydl Rust scaffold").size(34),
        text("Protocol-driven GUI shell with selectable downloader backends and SQLite."),
        status_line,
        row![queue_panel, side_panel]
            .spacing(20)
            .height(Length::Fill),
    ]
    .spacing(16)
    .padding(20)
    .height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn queue_controls(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let run_button = match app.snapshot.queue.status {
        QueueStatus::Running | QueueStatus::Backoff => {
            button("Pause").on_press(Message::RunOrPause)
        }
        QueueStatus::Paused => button("Resume").on_press(Message::RunOrPause),
        QueueStatus::Idle => button("Run").on_press(Message::RunOrPause),
    };

    column![
        row![
            text_input("Paste one or more Spotify URLs", &app.url_input)
                .on_input(Message::UrlInputChanged)
                .on_submit(Message::AddUrls)
                .padding(12)
                .width(Length::Fill),
            button("Add URLs").on_press(Message::AddUrls),
            run_button,
        ]
        .spacing(12),
        text(format!(
            "New jobs use destination `{}`, format `{}`, max parallel {}",
            blank_to_placeholder(&app.snapshot.settings.default_destination, "default"),
            blank_to_placeholder(&app.snapshot.settings.default_format, "default"),
            app.snapshot.settings.max_parallel
        ))
        .size(13),
    ]
    .spacing(12)
    .into()
}

fn settings_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let backend_row = row![
        backend_button("Fake", BackendKind::Fake, app.selected_backend),
        backend_button("External", BackendKind::External, app.selected_backend),
    ]
    .spacing(10);

    let helper_text = if app.selected_backend == BackendKind::External {
        "Set the `spotify-dl` executable path or command name. If left blank, the app will also look for a bundled `spotify-dl` next to the binary."
    } else {
        "Fake backend remains useful for smoke testing the Rust app without downloader setup."
    };

    container(
        column![
            text("Backend").size(24),
            text(format!(
                "Active: {}",
                app.snapshot.service_health.backend_name
            ))
            .size(14),
            text(format!("Status: {}", format_backend_status(&app.snapshot))).size(13),
            backend_row,
            text_input(
                "External executable, e.g. spotify-dl or C:\\tools\\spotify-dl.exe",
                &app.external_backend_input
            )
            .on_input(Message::ExternalBackendInputChanged)
            .padding(10)
            .width(Length::Fill),
            text_input(
                "Default destination, e.g. C:\\Music",
                &app.default_destination_input
            )
            .on_input(Message::DefaultDestinationInputChanged)
            .padding(10)
            .width(Length::Fill),
            text_input(
                "Default format, e.g. flac or mp3",
                &app.default_format_input
            )
            .on_input(Message::DefaultFormatInputChanged)
            .padding(10)
            .width(Length::Fill),
            text_input("Default max parallel", &app.max_parallel_input)
                .on_input(Message::MaxParallelInputChanged)
                .padding(10)
                .width(Length::Fill),
            text(helper_text).size(13),
            button("Save Settings").on_press(Message::SaveBackendSettings),
        ]
        .spacing(12),
    )
    .padding(16)
    .width(Length::Fill)
    .into()
}

fn backend_button<'a>(
    label: &'a str,
    backend: BackendKind,
    selected: BackendKind,
) -> Element<'a, Message> {
    let caption = if backend == selected {
        format!("[{label}]")
    } else {
        label.to_string()
    };

    button(text(caption))
        .on_press(Message::SelectBackend(backend))
        .into()
}

fn queue_list<'a>(jobs: &'a [JobRecord]) -> Element<'a, Message> {
    if jobs.is_empty() {
        return container(text("Queue is empty. Add URLs to create fake jobs."))
            .padding(16)
            .width(Length::Fill)
            .into();
    }

    let items = jobs.iter().fold(column!().spacing(12), |column, job| {
        let status = format!(
            "{:?}  {}%  {}",
            job.state, job.progress.percent, job.progress.detail
        );
        let meta = format!(
            "{} | {}",
            job.source_url,
            format_timestamp(job.updated_at_ms)
        );

        column.push(
            container(
                column![
                    row![
                        column![text(&job.label).size(20), text(status), text(meta).size(14)]
                            .spacing(6)
                            .width(Length::Fill),
                        button("Cancel").on_press(Message::CancelJob(job.id.clone())),
                    ]
                    .spacing(12),
                    text(job_totals_summary(job)).size(14),
                    job_items_panel(&job.items),
                ]
                .spacing(10),
            )
            .padding(14)
            .width(Length::Fill),
        )
    });

    scrollable(items).height(Length::Fill).into()
}

fn history_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let body = if app.snapshot.history.is_empty() {
        column![text("No history yet.")]
    } else {
        app.snapshot
            .history
            .iter()
            .fold(column!().spacing(10), |column, entry| {
                column.push(
                    container(
                        column![
                            row![
                                column![
                                    text(&entry.job.label),
                                    text(format!(
                                        "{:?} | {}",
                                        entry.job.state,
                                        format_timestamp(entry.job.updated_at_ms)
                                    ))
                                    .size(14),
                                ]
                                .spacing(4)
                                .width(Length::Fill),
                                button("Requeue")
                                    .on_press(Message::RequeueHistoryJob(entry.job.id.clone())),
                                button("Remove")
                                    .on_press(Message::RemoveHistoryJob(entry.job.id.clone())),
                            ]
                            .spacing(8),
                            text(history_inputs_preview(entry)).size(13),
                            text(job_totals_summary(&entry.job)).size(13),
                            job_items_panel(&entry.job.items),
                        ]
                        .spacing(6),
                    )
                    .padding(10),
                )
            })
    };

    container(
        column![
            row![
                text("History").size(24),
                button("Clear").on_press(Message::ClearHistory),
            ]
            .spacing(12),
            scrollable(body).height(260),
        ]
        .spacing(12),
    )
    .padding(16)
    .width(Length::Fill)
    .into()
}

fn history_inputs_preview(entry: &HistoryEntry) -> String {
    if entry.original_inputs.is_empty() {
        return "No original inputs recorded".to_string();
    }

    let first = &entry.original_inputs[0];
    if entry.original_inputs.len() == 1 {
        first.clone()
    } else {
        format!("{first} (+{} more)", entry.original_inputs.len() - 1)
    }
}

fn logs_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let body = if app.snapshot.logs.is_empty() {
        column![text("No logs yet.")]
    } else {
        app.snapshot
            .logs
            .iter()
            .rev()
            .fold(column!().spacing(8), |column, entry| {
                let job_suffix = entry
                    .job_id
                    .as_ref()
                    .map(|job_id| format!(" [{job_id}]"))
                    .unwrap_or_default();

                column.push(text(format!(
                    "{} {:?}/{:?}{} {}",
                    format_timestamp(entry.timestamp_ms),
                    entry.scope,
                    entry.level,
                    job_suffix,
                    entry.message
                )))
            })
    };

    container(
        column![
            text("Log Panel").size(24),
            scrollable(body).height(Length::Fill)
        ]
        .spacing(12),
    )
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn job_totals_summary(job: &JobRecord) -> String {
    format!(
        "Items: {} total, {} complete, {} failed, {} cancelled | Outputs: {} written, {} skipped | Flags: {}",
        job.totals.items_total,
        job.totals.items_completed,
        job.totals.items_failed,
        job.totals.items_cancelled,
        job.totals.outputs_written,
        job.totals.outputs_skipped,
        job.totals.flagged_items
    )
}

fn job_items_panel<'a>(items: &'a [DownloadItem]) -> Element<'a, Message> {
    if items.is_empty() {
        return text("No items").into();
    }

    let content = items.iter().fold(column!().spacing(8), |column, item| {
        let status = format!(
            "{:?} | {}% | {}",
            item.state, item.progress.percent, item.progress.detail
        );
        let failure = item
            .failure_reason
            .as_ref()
            .map(|reason| format!("Failure: {}", reason.message))
            .unwrap_or_default();
        let output = item
            .outputs
            .first()
            .map(|output| format!("Output: {}", output.final_path))
            .unwrap_or_else(|| "Output: none".to_string());
        let extra_outputs = if item.outputs.len() > 1 {
            format!(" (+{} more)", item.outputs.len() - 1)
        } else {
            String::new()
        };
        let flag_summary = if item.flags.is_empty() {
            "Flags: none".to_string()
        } else {
            format!(
                "Flags: {}",
                item.flags
                    .iter()
                    .map(|flag| flag.message.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        };

        let mut item_column = column![
            text(format!("- {}", item.label)).size(15),
            text(status).size(13),
            text(format!("{output}{extra_outputs}")).size(13),
            text(flag_summary).size(13),
        ]
        .spacing(3);

        if !failure.is_empty() {
            item_column = item_column.push(text(failure).size(13));
        }

        column.push(container(item_column).padding(8).width(Length::Fill))
    });

    container(content).width(Length::Fill).into()
}

fn default_database_path() -> PathBuf {
    if let Some(project_dirs) = ProjectDirs::from("dev", "spotifydl", "spotifydl-rust") {
        return project_dirs.data_dir().join("spotifydl-rust.sqlite");
    }

    PathBuf::from(".spotifydl-rust/spotifydl-rust.sqlite")
}

fn format_timestamp(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown time".to_string())
}

fn blank_to_placeholder<'a>(value: &'a str, placeholder: &'a str) -> &'a str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        placeholder
    } else {
        trimmed
    }
}

fn format_backend_status(snapshot: &AppSnapshot) -> String {
    let prefix = if snapshot.service_health.backend_ready {
        "Ready"
    } else {
        "Not ready"
    };

    let detail = snapshot.service_health.backend_status_message.trim();
    if detail.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {detail}")
    }
}

fn sync_settings_inputs(app: &mut SpotifydlGuiApp) {
    app.selected_backend = app.snapshot.settings.preferred_backend;
    app.external_backend_input = app.snapshot.settings.external_backend_executable.clone();
    app.default_destination_input = app.snapshot.settings.default_destination.clone();
    app.default_format_input = app.snapshot.settings.default_format.clone();
    app.max_parallel_input = app.snapshot.settings.max_parallel.to_string();
}
