use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use chrono::{Local, TimeZone};
use directories::ProjectDirs;
use iced::{
    Background, Color, Element, Length, Shadow, Subscription, Task, Theme, Vector, application,
    border, time,
    widget::{button, column, container, progress_bar, row, scrollable, text, text_input, toggler},
};
use spotifydl_protocol::{
    AppSnapshot, BackendKind, DownloadItem, HistoryEntry, ItemState, JobId, JobRecord, LogEntry,
    QueueStatus, ServiceCommand, ThemeMode,
};
use spotifydl_service::SpotifydlService;

pub fn main() -> iced::Result {
    application("spotifydl", update, view)
        .subscription(subscription)
        .theme(|app: &SpotifydlGuiApp| app_theme(app.theme_mode))
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
    theme_mode: ThemeMode,
    side_tab: SideTab,
    displayed_progress: HashMap<JobId, f32>,
    selected_job: Option<SelectedJob>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedJob {
    Queue(JobId),
    History(JobId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SideTab {
    Overview,
    History,
    Settings,
    Logs,
}

#[derive(Debug, Clone, Copy)]
enum ButtonTone {
    Primary,
    Secondary,
    Ghost,
    Danger,
    Tab(bool),
}

#[derive(Debug, Clone, Copy)]
enum ChipTone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone)]
enum Message {
    UrlInputChanged(String),
    AddUrls,
    RunOrPause,
    CancelJob(JobId),
    RetryFailedItems(JobId),
    RemoveQueueJob(JobId),
    OpenPath(String),
    OpenContainingFolder(String),
    RequeueHistoryJob(JobId),
    RemoveHistoryJob(JobId),
    ClearHistory,
    SelectBackend(BackendKind),
    ExternalBackendInputChanged(String),
    DefaultDestinationInputChanged(String),
    DefaultFormatInputChanged(String),
    MaxParallelInputChanged(String),
    ThemeModeToggled(bool),
    SelectSideTab(SideTab),
    SelectQueueJob(JobId),
    SelectHistoryJob(JobId),
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
    let theme_mode = snapshot.settings.theme_mode;

    let mut app = SpotifydlGuiApp {
        service,
        snapshot,
        url_input: String::new(),
        selected_backend,
        external_backend_input,
        default_destination_input,
        default_format_input,
        max_parallel_input,
        theme_mode,
        side_tab: SideTab::Overview,
        displayed_progress: HashMap::new(),
        selected_job: None,
        last_error: None,
    };
    normalize_selected_job(&mut app);
    sync_displayed_progress(&mut app);

    (app, Task::none())
}

fn update(app: &mut SpotifydlGuiApp, message: Message) -> Task<Message> {
    match message {
        Message::UrlInputChanged(value) => {
            app.url_input = value;
        }
        Message::AddUrls => {
            let should_autostart = matches!(app.snapshot.queue.status, QueueStatus::Idle);
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

                if should_autostart {
                    dispatch(app, ServiceCommand::StartQueue);
                }
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
        Message::RetryFailedItems(job_id) => {
            dispatch(app, ServiceCommand::RetryFailedItems { job_id });
        }
        Message::RemoveQueueJob(job_id) => {
            dispatch(
                app,
                ServiceCommand::RemoveJob {
                    job_id,
                    from_history: false,
                },
            );
        }
        Message::OpenPath(path) => {
            if let Err(error) = open_path(&path) {
                app.last_error = Some(error);
            } else {
                app.last_error = None;
            }
        }
        Message::OpenContainingFolder(path) => {
            if let Err(error) = open_containing_folder(&path) {
                app.last_error = Some(error);
            } else {
                app.last_error = None;
            }
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
        Message::ThemeModeToggled(is_dark) => {
            let theme_mode = if is_dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            };
            app.theme_mode = theme_mode;

            let mut settings = app.snapshot.settings.clone();
            settings.theme_mode = theme_mode;
            let _ = dispatch(app, ServiceCommand::UpdateSettings { settings });
        }
        Message::SelectSideTab(tab) => {
            app.side_tab = tab;
        }
        Message::SelectQueueJob(job_id) => {
            app.side_tab = SideTab::Overview;
            app.selected_job = Some(SelectedJob::Queue(job_id));
        }
        Message::SelectHistoryJob(job_id) => {
            app.side_tab = SideTab::Overview;
            app.selected_job = Some(SelectedJob::History(job_id));
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
            animate_displayed_progress(app);
        }
    }

    Task::none()
}

fn dispatch(app: &mut SpotifydlGuiApp, command: ServiceCommand) -> bool {
    match app.service.dispatch(command) {
        Ok(_) => {
            app.snapshot = app.service.snapshot().clone();
            normalize_selected_job(app);
            sync_displayed_progress(app);
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
        text("Downloads").size(28),
        queue_controls(app),
        queue_list(app),
    ]
    .spacing(16)
    .width(Length::FillPortion(3));

    let side_panel = side_panel(app);

    let content = column![
        hero_banner(app),
        row![queue_panel, side_panel]
            .spacing(20)
            .height(Length::Fill),
    ]
    .spacing(20)
    .padding(24)
    .height(Length::Fill);

    container(content)
        .style(app_shell_style)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn queue_controls(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let run_button = match app.snapshot.queue.status {
        QueueStatus::Running | QueueStatus::Backoff => {
            action_button("Pause", Message::RunOrPause, ButtonTone::Secondary)
        }
        QueueStatus::Paused => action_button("Resume", Message::RunOrPause, ButtonTone::Secondary),
        QueueStatus::Idle => action_button("Start", Message::RunOrPause, ButtonTone::Secondary),
    };

    container(
        column![
            text("Drop in a link").size(18),
            row![
                text_input(
                    "Paste a Spotify song, album, or playlist link",
                    &app.url_input
                )
                .on_input(Message::UrlInputChanged)
                .on_submit(Message::AddUrls)
                .padding(14)
                .style(input_style)
                .width(Length::Fill),
                action_button("Download", Message::AddUrls, ButtonTone::Primary),
                run_button,
            ]
            .spacing(12),
            text(format!(
                "Downloads save to `{}` as `{}` with up to {} item(s) at once",
                blank_to_placeholder(
                    &app.snapshot.settings.default_destination,
                    "your default folder"
                ),
                blank_to_placeholder(&app.snapshot.settings.default_format, "your default format"),
                app.snapshot.settings.max_parallel
            ))
            .size(13),
        ]
        .spacing(14),
    )
    .style(card_style)
    .padding(20)
    .into()
}

fn side_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let tabs = row![
        side_tab_button("Details", SideTab::Overview, app.side_tab),
        side_tab_button("History", SideTab::History, app.side_tab),
        side_tab_button("Settings", SideTab::Settings, app.side_tab),
        side_tab_button("Activity", SideTab::Logs, app.side_tab),
    ]
    .spacing(8);

    let body = match app.side_tab {
        SideTab::Overview => selected_job_panel(app),
        SideTab::History => history_panel(app),
        SideTab::Settings => settings_panel(app),
        SideTab::Logs => logs_panel(app),
    };

    container(
        column![tabs, scrollable(body).height(Length::Fill),]
            .spacing(16)
            .height(Length::Fill),
    )
    .style(card_style)
    .padding(16)
    .width(Length::FillPortion(2))
    .height(Length::Fill)
    .into()
}

fn side_tab_button<'a>(label: &'a str, tab: SideTab, selected: SideTab) -> Element<'a, Message> {
    let caption = if tab == selected {
        format!("[{label}]")
    } else {
        label.to_string()
    };

    action_button(
        caption,
        Message::SelectSideTab(tab),
        ButtonTone::Tab(tab == selected),
    )
}

fn settings_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let backend_row = row![
        backend_button("Fake", BackendKind::Fake, app.selected_backend),
        backend_button("External", BackendKind::External, app.selected_backend),
    ]
    .spacing(10);

    let helper_text = if app.selected_backend == BackendKind::External {
        "Choose where the downloader lives. Leave it blank to use a bundled `spotify-dl` next to the app when available."
    } else {
        "Use the fake backend only for testing the app without downloading files."
    };

    let settings_dirty = settings_dirty(app);
    let save_note = if settings_dirty {
        "You have unsaved changes"
    } else {
        "Saved settings are active"
    };
    let validation_note = format!(
        "Default save location `{}`, format `{}`, max parallel {}",
        blank_to_placeholder(
            &app.snapshot.settings.default_destination,
            "your default folder"
        ),
        blank_to_placeholder(&app.snapshot.settings.default_format, "your default format"),
        app.snapshot.settings.max_parallel
    );
    let active_change_note = if app.snapshot.queue.active_job_id.is_some() {
        "Changes save right away, but an active download keeps using its current backend."
    } else {
        "Changes apply immediately while downloads are idle."
    };

    column![
        text("Settings").size(24),
        container(
            column![
                row![
                    text("Appearance").size(15),
                    row![
                        text("Light").size(13),
                        toggler(matches!(app.theme_mode, ThemeMode::Dark))
                            .on_toggle(Message::ThemeModeToggled),
                        text("Dark").size(13),
                    ]
                    .spacing(10),
                ]
                .spacing(12),
                text(format!(
                    "Using: {}",
                    app.snapshot.service_health.backend_name
                ))
                .size(14),
                text(format!("Status: {}", format_backend_status(&app.snapshot))).size(13),
                backend_row,
                text_input(
                    "Downloader app, e.g. spotify-dl or C:\\tools\\spotify-dl.exe",
                    &app.external_backend_input
                )
                .on_input(Message::ExternalBackendInputChanged)
                .padding(12)
                .style(input_style)
                .width(Length::Fill),
                text_input(
                    "Save downloads to, e.g. C:\\Music",
                    &app.default_destination_input
                )
                .on_input(Message::DefaultDestinationInputChanged)
                .padding(12)
                .style(input_style)
                .width(Length::Fill),
                text_input(
                    "Default format, e.g. flac or mp3",
                    &app.default_format_input
                )
                .on_input(Message::DefaultFormatInputChanged)
                .padding(12)
                .style(input_style)
                .width(Length::Fill),
                text_input(
                    "How many items to download at once",
                    &app.max_parallel_input
                )
                .on_input(Message::MaxParallelInputChanged)
                .padding(12)
                .style(input_style)
                .width(Length::Fill),
                text(save_note).size(13),
                text(validation_note).size(13),
                text(helper_text).size(13),
                text(active_change_note).size(13),
                action_button("Save", Message::SaveBackendSettings, ButtonTone::Primary),
            ]
            .spacing(12),
        )
        .style(soft_card_style)
        .padding(16),
    ]
    .spacing(12)
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

    action_button(
        caption,
        Message::SelectBackend(backend),
        ButtonTone::Tab(backend == selected),
    )
}

fn queue_list(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let jobs = &app.snapshot.queue.jobs;

    if jobs.is_empty() {
        return container(text("No downloads yet. Paste a link above to get started."))
            .style(soft_card_style)
            .padding(16)
            .width(Length::Fill)
            .into();
    }

    let active_job_id = app.snapshot.queue.active_job_id.as_ref();
    let items = jobs.iter().fold(column!().spacing(12), |column, job| {
        let issue_summary = queue_issue_summary(&app.snapshot, job);
        let displayed_progress = app
            .displayed_progress
            .get(&job.id)
            .copied()
            .unwrap_or(f32::from(job.progress.percent));
        let job_action = if active_job_id == Some(&job.id) {
            action_button(
                "Cancel Active",
                Message::CancelJob(job.id.clone()),
                ButtonTone::Danger,
            )
        } else {
            action_button(
                "Remove",
                Message::RemoveQueueJob(job.id.clone()),
                ButtonTone::Ghost,
            )
        };
        let retry_button = if job_has_retryable_items(job) {
            Some(action_button(
                "Retry Failed",
                Message::RetryFailedItems(job.id.clone()),
                ButtonTone::Secondary,
            ))
        } else {
            None
        };

        let actions = retry_button.into_iter().fold(
            row![
                action_button(
                    "Details",
                    Message::SelectQueueJob(job.id.clone()),
                    ButtonTone::Ghost
                ),
                job_action,
            ]
            .spacing(12),
            |row, button| row.push(button),
        );

        let progress_row = row![
            progress_bar(0.0..=100.0, displayed_progress)
                .height(10)
                .style(progress_style)
                .width(Length::Fill),
            text(queue_progress_caption(&app.snapshot, job)).size(13),
        ]
        .spacing(12);

        let issue_panel = issue_summary.map(|summary| {
            container(column![text("Needs attention").size(13), text(summary).size(13),].spacing(4))
                .style(soft_card_style)
                .padding(12)
                .width(Length::Fill)
        });

        let mut card_body = column![
            row![
                column![text(&job.label).size(20), text(&job.source_url).size(13),]
                    .spacing(6)
                    .width(Length::Fill),
                actions,
            ]
            .spacing(12),
            progress_row,
        ]
        .spacing(10);

        if let Some(issue_panel) = issue_panel {
            card_body = card_body.push(issue_panel);
        }

        column.push(
            container(card_body)
                .style(move |theme| {
                    job_card_style(
                        theme,
                        is_selected_job(app, &job.id),
                        active_job_id == Some(&job.id),
                    )
                })
                .padding(14)
                .width(Length::Fill),
        )
    });

    scrollable(items).height(Length::Fill).into()
}

fn history_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let body = if app.snapshot.history.is_empty() {
        column![
            container(text("No completed downloads yet."))
                .style(soft_card_style)
                .padding(16)
        ]
    } else {
        app.snapshot
            .history
            .iter()
            .fold(column!().spacing(10), |column, entry| {
                let status_summary = history_job_status_summary(&app.snapshot, &entry.job);
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
                                action_button(
                                    "Details",
                                    Message::SelectHistoryJob(entry.job.id.clone()),
                                    ButtonTone::Ghost
                                ),
                                action_button(
                                    "Requeue",
                                    Message::RequeueHistoryJob(entry.job.id.clone()),
                                    ButtonTone::Secondary
                                ),
                                action_button(
                                    "Remove",
                                    Message::RemoveHistoryJob(entry.job.id.clone()),
                                    ButtonTone::Ghost
                                ),
                            ]
                            .spacing(8),
                            text(history_inputs_preview(entry)).size(13),
                            text(status_summary).size(13),
                            text(job_totals_summary(&entry.job)).size(13),
                            job_items_preview(&entry.job.items),
                        ]
                        .spacing(6),
                    )
                    .style(move |theme| {
                        job_card_style(theme, is_history_selected(app, &entry.job.id), false)
                    })
                    .padding(10),
                )
            })
    };

    container(
        column![
            row![
                text("Recent Downloads").size(24),
                action_button("Clear", Message::ClearHistory, ButtonTone::Ghost),
            ]
            .spacing(12),
            body,
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
        column![
            container(text("No activity yet."))
                .style(soft_card_style)
                .padding(16)
        ]
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

                column.push(
                    container(text(format!(
                        "{} {:?}/{:?}{} {}",
                        format_timestamp(entry.timestamp_ms),
                        entry.scope,
                        entry.level,
                        job_suffix,
                        entry.message
                    )))
                    .style(soft_card_style)
                    .padding(12),
                )
            })
    };

    container(column![text("Activity").size(24), body].spacing(12))
        .padding(16)
        .width(Length::Fill)
        .into()
}

fn selected_job_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let Some(selected) = selected_job_record(app) else {
        return container(
            column![
                text("Details").size(24),
                text(
                    "Select a download to see its files, progress, problems, and recent activity."
                )
                .size(13),
            ]
            .spacing(10),
        )
        .style(soft_card_style)
        .padding(16)
        .width(Length::Fill)
        .into();
    };

    let section_label = match selected {
        SelectedJobView::Queue { .. } => "Queue",
        SelectedJobView::History { .. } => "History",
    };
    let job = selected.job();
    let original_inputs = selected.original_inputs();
    let failure_message = job.error_message.as_deref().unwrap_or("none");
    let job_logs = app
        .snapshot
        .logs
        .iter()
        .rev()
        .filter(|entry| entry.job_id.as_ref() == Some(&job.id))
        .take(6)
        .collect::<Vec<_>>();

    let logs_column = if job_logs.is_empty() {
        column![text("Recent logs: none").size(13)]
    } else {
        job_logs.into_iter().fold(
            column![text("Recent logs").size(14)].spacing(4),
            |column, entry| column.push(text(format_job_log(entry)).size(13)),
        )
    };

    let inputs_text = if original_inputs.is_empty() {
        "Added links: none recorded".to_string()
    } else {
        format!(
            "Added links: {}",
            original_inputs
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        )
    };

    container(
        column![
            text("Details").size(24),
            text(format!("{section_label} | {:?}", job.state)).size(14),
            text(&job.label).size(18),
            text(inputs_text).size(13),
            text(format!("Link: {}", job.source_url)).size(13),
            text(job_timestamps_summary(job)).size(13),
            text(format!(
                "Options: destination `{}`, format `{}`, max parallel {}",
                blank_to_placeholder(&job.options.destination, "your default folder"),
                blank_to_placeholder(&job.options.format, "your default format"),
                job.options.max_parallel
            ))
            .size(13),
            text(format!("Problem: {failure_message}")).size(13),
            text(job_totals_summary(job)).size(13),
            selected_job_actions(app, &selected),
            selected_job_outputs_panel(job),
            selected_job_failures_panel(job),
            job_items_panel(&job.items),
            logs_column,
        ]
        .spacing(10),
    )
    .style(soft_card_style)
    .padding(16)
    .width(Length::Fill)
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

fn job_items_preview<'a>(items: &'a [DownloadItem]) -> Element<'a, Message> {
    if items.is_empty() {
        return text("Items: none").size(13).into();
    }

    let preview = items.iter().take(2).fold(
        column![text("Item preview").size(13)].spacing(4),
        |column, item| {
            let output_count = item.outputs.len();
            let summary = if let Some(reason) = &item.failure_reason {
                format!("{} | {:?} | {}", item.label, item.state, reason.message)
            } else if output_count > 0 {
                format!(
                    "{} | {:?} | {} output(s)",
                    item.label, item.state, output_count
                )
            } else {
                format!(
                    "{} | {:?} | {}%",
                    item.label, item.state, item.progress.percent
                )
            };

            column.push(text(summary).size(13))
        },
    );

    let remaining = items.len().saturating_sub(2);
    let preview = if remaining > 0 {
        preview.push(text(format!("+{remaining} more item(s)")).size(13))
    } else {
        preview
    };

    container(preview)
        .style(soft_card_style)
        .padding(12)
        .width(Length::Fill)
        .into()
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

        column.push(
            container(item_column)
                .style(soft_card_style)
                .padding(10)
                .width(Length::Fill),
        )
    });

    container(content).width(Length::Fill).into()
}

enum SelectedJobView<'a> {
    Queue { job: &'a JobRecord },
    History { entry: &'a HistoryEntry },
}

impl<'a> SelectedJobView<'a> {
    fn job(&self) -> &'a JobRecord {
        match self {
            Self::Queue { job } => job,
            Self::History { entry } => &entry.job,
        }
    }

    fn original_inputs(&self) -> &'a [String] {
        match self {
            Self::Queue { .. } => &[],
            Self::History { entry } => &entry.original_inputs,
        }
    }
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

fn queue_state_summary(snapshot: &AppSnapshot) -> String {
    let recovery_suffix = snapshot
        .service_health
        .last_recovery_at_ms
        .map(|timestamp| format!(" | Last recovery {}", format_timestamp(timestamp)))
        .unwrap_or_default();

    match snapshot.queue.status {
        QueueStatus::Backoff => format!(
            "Waiting briefly: {} ({} ms remaining){}",
            blank_to_placeholder(&snapshot.service_health.backoff.reason, "waiting"),
            snapshot.service_health.backoff.remaining_ms,
            recovery_suffix
        ),
        QueueStatus::Paused if snapshot.queue.active_job_id.is_some() => format!(
            "Paused. The current download is still finishing its last step{}",
            recovery_suffix
        ),
        QueueStatus::Paused => format!("Paused{}", recovery_suffix),
        QueueStatus::Running => {
            if let Some(job_id) = &snapshot.queue.active_job_id {
                format!("Downloading now: {job_id}{recovery_suffix}")
            } else {
                format!("Downloading now{recovery_suffix}")
            }
        }
        QueueStatus::Idle => {
            if snapshot.queue.jobs.is_empty() {
                format!("Ready for a link{recovery_suffix}")
            } else {
                format!(
                    "{} download(s) waiting{recovery_suffix}",
                    snapshot.queue.jobs.len()
                )
            }
        }
    }
}

fn queue_progress_caption(snapshot: &AppSnapshot, job: &JobRecord) -> String {
    if snapshot.queue.active_job_id.as_ref() == Some(&job.id)
        && snapshot.queue.status == QueueStatus::Backoff
    {
        return "Waiting briefly".to_string();
    }

    match job.state {
        spotifydl_protocol::JobState::Queued => "Queued".to_string(),
        spotifydl_protocol::JobState::Paused => "Paused".to_string(),
        spotifydl_protocol::JobState::Running => format!("{}%", job.progress.percent),
        spotifydl_protocol::JobState::Completed => "Done".to_string(),
        spotifydl_protocol::JobState::Failed => "Issue found".to_string(),
        spotifydl_protocol::JobState::Cancelled => "Cancelled".to_string(),
    }
}

fn queue_issue_summary(snapshot: &AppSnapshot, job: &JobRecord) -> Option<String> {
    let _ = snapshot;

    if let Some(message) = job.error_message.as_deref() {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if job.totals.items_failed > 0 {
        return Some(format!(
            "{} item(s) failed during this download.",
            job.totals.items_failed
        ));
    }

    if job.totals.items_cancelled > 0 {
        return Some(format!(
            "{} item(s) were cancelled before finishing.",
            job.totals.items_cancelled
        ));
    }

    if job.totals.flagged_items > 0 {
        return Some(format!(
            "{} item(s) need review after download.",
            job.totals.flagged_items
        ));
    }

    None
}

fn hero_banner(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let queue_tone = match app.snapshot.queue.status {
        QueueStatus::Running => ChipTone::Success,
        QueueStatus::Backoff => ChipTone::Warning,
        QueueStatus::Paused => ChipTone::Neutral,
        QueueStatus::Idle => ChipTone::Accent,
    };
    let backend_tone = if app.snapshot.service_health.backend_ready {
        ChipTone::Accent
    } else {
        ChipTone::Danger
    };

    let mut chips = row![
        status_chip(format_backend_status(&app.snapshot), backend_tone),
        status_chip(queue_state_summary(&app.snapshot), queue_tone),
    ]
    .spacing(10);

    if let Some(error) = &app.last_error {
        chips = chips.push(status_chip(format!("Problem: {error}"), ChipTone::Danger));
    }

    container(
        column![
            text("spotifydl").size(36),
            text("Paste a Spotify link and your download starts with your saved settings.")
                .size(15),
            chips,
        ]
        .spacing(14),
    )
    .style(hero_card_style)
    .padding(24)
    .width(Length::Fill)
    .into()
}

fn action_button(
    label: impl Into<String>,
    message: Message,
    tone: ButtonTone,
) -> Element<'static, Message> {
    button(text(label.into()).size(14))
        .padding([10, 16])
        .style(move |theme, status| button_style(theme, tone, status))
        .on_press(message)
        .into()
}

fn status_chip<'a>(label: String, tone: ChipTone) -> Element<'a, Message> {
    container(text(label).size(13))
        .style(move |theme| chip_style(theme, tone))
        .padding([8, 12])
        .into()
}

fn sync_displayed_progress(app: &mut SpotifydlGuiApp) {
    app.displayed_progress
        .retain(|job_id, _| app.snapshot.queue.jobs.iter().any(|job| &job.id == job_id));

    for job in &app.snapshot.queue.jobs {
        app.displayed_progress
            .entry(job.id.clone())
            .or_insert(f32::from(job.progress.percent));
    }
}

fn animate_displayed_progress(app: &mut SpotifydlGuiApp) {
    for job in &app.snapshot.queue.jobs {
        let target = f32::from(job.progress.percent);
        let displayed = app
            .displayed_progress
            .entry(job.id.clone())
            .or_insert(target);

        if job.state.is_terminal() || target <= *displayed {
            *displayed = target;
            continue;
        }

        let delta = target - *displayed;
        let step = (delta * 0.18).max(0.6);
        *displayed = (*displayed + step).min(target);
    }
}

fn app_shell_style(theme: &Theme) -> iced::widget::container::Style {
    let base = theme.palette();
    let palette = theme.extended_palette();
    let background = if theme_is_dark(theme) {
        base.background
    } else {
        palette.background.weak.color
    };

    iced::widget::container::Style::default()
        .background(background)
        .color(base.text)
}

fn hero_card_style(theme: &Theme) -> iced::widget::container::Style {
    if theme_is_dark(theme) {
        let base = theme.palette();

        return iced::widget::container::Style::default()
            .background(surface_color(theme, 0.08))
            .border(
                border::rounded(28)
                    .width(1)
                    .color(with_alpha(blend(base.primary, Color::WHITE, 0.15), 0.28)),
            )
            .shadow(Shadow {
                color: with_alpha(Color::BLACK, 0.35),
                offset: Vector::new(0.0, 14.0),
                blur_radius: 34.0,
            });
    }

    let palette = theme.extended_palette();

    iced::widget::container::Style::default()
        .background(palette.background.base.color)
        .border(
            border::rounded(28)
                .width(1)
                .color(with_alpha(palette.background.strong.color, 0.35)),
        )
        .shadow(Shadow {
            color: with_alpha(palette.background.base.text, 0.10),
            offset: Vector::new(0.0, 12.0),
            blur_radius: 32.0,
        })
}

fn card_style(theme: &Theme) -> iced::widget::container::Style {
    if theme_is_dark(theme) {
        let base = theme.palette();

        return iced::widget::container::Style::default()
            .background(surface_color(theme, 0.055))
            .border(
                border::rounded(24)
                    .width(1)
                    .color(with_alpha(blend(base.primary, Color::WHITE, 0.10), 0.18)),
            )
            .shadow(Shadow {
                color: with_alpha(Color::BLACK, 0.26),
                offset: Vector::new(0.0, 10.0),
                blur_radius: 26.0,
            });
    }

    let palette = theme.extended_palette();

    iced::widget::container::Style::default()
        .background(palette.background.base.color)
        .border(
            border::rounded(24)
                .width(1)
                .color(with_alpha(palette.background.strong.color, 0.30)),
        )
        .shadow(Shadow {
            color: with_alpha(palette.background.base.text, 0.08),
            offset: Vector::new(0.0, 10.0),
            blur_radius: 24.0,
        })
}

fn soft_card_style(theme: &Theme) -> iced::widget::container::Style {
    if theme_is_dark(theme) {
        let base = theme.palette();

        return iced::widget::container::Style::default()
            .background(surface_color(theme, 0.09))
            .border(
                border::rounded(20)
                    .width(1)
                    .color(with_alpha(blend(base.primary, Color::WHITE, 0.08), 0.14)),
            );
    }

    let palette = theme.extended_palette();

    iced::widget::container::Style::default()
        .background(palette.background.weak.color)
        .border(
            border::rounded(20)
                .width(1)
                .color(with_alpha(palette.background.strong.color, 0.22)),
        )
}

fn chip_style(theme: &Theme, tone: ChipTone) -> iced::widget::container::Style {
    let palette = theme.extended_palette();
    let (background, text_color, border_color) = match tone {
        ChipTone::Neutral => (
            palette.background.weak.color,
            palette.background.base.text,
            with_alpha(palette.background.strong.color, 0.30),
        ),
        ChipTone::Accent => (
            palette.primary.weak.color,
            palette.primary.strong.text,
            with_alpha(palette.primary.strong.color, 0.35),
        ),
        ChipTone::Success => (
            palette.success.weak.color,
            palette.success.strong.text,
            with_alpha(palette.success.strong.color, 0.35),
        ),
        ChipTone::Warning => (
            palette.secondary.weak.color,
            palette.secondary.strong.text,
            with_alpha(palette.secondary.strong.color, 0.35),
        ),
        ChipTone::Danger => (
            palette.danger.weak.color,
            palette.danger.strong.text,
            with_alpha(palette.danger.strong.color, 0.35),
        ),
    };

    iced::widget::container::Style::default()
        .background(background)
        .color(text_color)
        .border(border::rounded(999).width(1).color(border_color))
}

fn progress_style(theme: &Theme) -> iced::widget::progress_bar::Style {
    let palette = theme.extended_palette();
    let background = if theme_is_dark(theme) {
        surface_color(theme, 0.14)
    } else {
        with_alpha(palette.background.strong.color, 0.22)
    };

    iced::widget::progress_bar::Style {
        background: Background::Color(background),
        bar: Background::Color(palette.primary.strong.color),
        border: border::rounded(999),
    }
}

fn button_style(
    theme: &Theme,
    tone: ButtonTone,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    let palette = theme.extended_palette();
    let (background, text_color, border_color, shadow_color) = match tone {
        ButtonTone::Primary => (
            palette.primary.strong.color,
            palette.primary.strong.text,
            palette.primary.strong.color,
            with_alpha(palette.primary.strong.color, 0.24),
        ),
        ButtonTone::Secondary => (
            palette.secondary.weak.color,
            palette.secondary.base.text,
            with_alpha(palette.secondary.strong.color, 0.25),
            with_alpha(palette.secondary.strong.color, 0.14),
        ),
        ButtonTone::Ghost => (
            palette.background.base.color,
            palette.background.base.text,
            with_alpha(palette.background.strong.color, 0.22),
            with_alpha(palette.background.base.text, 0.06),
        ),
        ButtonTone::Danger => (
            palette.danger.weak.color,
            palette.danger.strong.text,
            with_alpha(palette.danger.strong.color, 0.30),
            with_alpha(palette.danger.strong.color, 0.12),
        ),
        ButtonTone::Tab(selected) => {
            if selected {
                (
                    palette.primary.weak.color,
                    palette.primary.strong.text,
                    with_alpha(palette.primary.strong.color, 0.25),
                    with_alpha(palette.primary.strong.color, 0.14),
                )
            } else {
                (
                    palette.background.weak.color,
                    palette.background.base.text,
                    with_alpha(palette.background.strong.color, 0.18),
                    with_alpha(palette.background.base.text, 0.04),
                )
            }
        }
    };

    let mut style = iced::widget::button::Style {
        background: Some(Background::Color(background)),
        text_color,
        border: border::rounded(18).width(1).color(border_color),
        shadow: Shadow {
            color: shadow_color,
            offset: Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
    };

    match status {
        iced::widget::button::Status::Hovered => {
            style.shadow.offset = Vector::new(0.0, 6.0);
            style.shadow.blur_radius = 16.0;
        }
        iced::widget::button::Status::Pressed => {
            style.shadow.offset = Vector::new(0.0, 2.0);
            style.shadow.blur_radius = 8.0;
        }
        iced::widget::button::Status::Disabled => {
            style.background = Some(Background::Color(palette.background.weak.color));
            style.text_color = with_alpha(palette.background.base.text, 0.45);
            style.shadow = Shadow::default();
        }
        iced::widget::button::Status::Active => {}
    }

    style
}

fn input_style(
    theme: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    let palette = theme.extended_palette();
    let mut style = iced::widget::text_input::Style {
        background: Background::Color(if theme_is_dark(theme) {
            surface_color(theme, 0.10)
        } else {
            palette.background.base.color
        }),
        border: border::rounded(18)
            .width(1)
            .color(with_alpha(palette.background.strong.color, 0.26)),
        icon: with_alpha(palette.background.base.text, 0.55),
        placeholder: with_alpha(palette.background.base.text, 0.50),
        value: palette.background.base.text,
        selection: with_alpha(palette.primary.base.color, 0.22),
    };

    match status {
        iced::widget::text_input::Status::Hovered => {
            style.border.color = with_alpha(palette.background.strong.color, 0.42);
        }
        iced::widget::text_input::Status::Focused => {
            style.border.color = palette.primary.strong.color;
        }
        iced::widget::text_input::Status::Disabled => {
            style.background = Background::Color(palette.background.weak.color);
            style.value = with_alpha(palette.background.base.text, 0.45);
        }
        iced::widget::text_input::Status::Active => {}
    }

    style
}

fn job_card_style(theme: &Theme, selected: bool, active: bool) -> iced::widget::container::Style {
    let palette = theme.extended_palette();
    let background = if theme_is_dark(theme) {
        if active {
            blend(
                surface_color(theme, 0.12),
                palette.primary.strong.color,
                0.10,
            )
        } else {
            surface_color(theme, 0.065)
        }
    } else if active {
        palette.primary.weak.color
    } else {
        palette.background.base.color
    };
    let border_color = if selected {
        palette.primary.strong.color
    } else if active {
        with_alpha(palette.primary.strong.color, 0.40)
    } else {
        with_alpha(palette.background.strong.color, 0.24)
    };

    iced::widget::container::Style::default()
        .background(background)
        .border(
            border::rounded(22)
                .width(if selected { 2.0 } else { 1.0 })
                .color(border_color),
        )
        .shadow(Shadow {
            color: with_alpha(palette.background.base.text, 0.08),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 20.0,
        })
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

fn app_theme(mode: ThemeMode) -> Theme {
    match mode {
        ThemeMode::Light => Theme::custom("spotifydl-light".to_string(), light_palette()),
        ThemeMode::Dark => Theme::custom("spotifydl-dark".to_string(), dark_palette()),
    }
}

fn blend(a: Color, b: Color, amount: f32) -> Color {
    let t = amount.clamp(0.0, 1.0);

    Color::from_rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a + (b.a - a.a) * t,
    )
}

fn surface_color(theme: &Theme, lift: f32) -> Color {
    blend(theme.palette().background, Color::WHITE, lift)
}

fn theme_is_dark(theme: &Theme) -> bool {
    let background = theme.palette().background;
    (background.r + background.g + background.b) / 3.0 < 0.5
}

fn light_palette() -> iced::theme::Palette {
    iced::theme::Palette {
        background: Color::from_rgb8(246, 241, 235),
        text: Color::from_rgb8(34, 29, 24),
        primary: Color::from_rgb8(232, 124, 32),
        success: Color::from_rgb8(73, 140, 92),
        danger: Color::from_rgb8(198, 79, 71),
    }
}

fn dark_palette() -> iced::theme::Palette {
    iced::theme::Palette {
        background: Color::from_rgb8(11, 10, 9),
        text: Color::from_rgb8(244, 236, 227),
        primary: Color::from_rgb8(242, 147, 52),
        success: Color::from_rgb8(102, 176, 124),
        danger: Color::from_rgb8(229, 118, 106),
    }
}

fn is_selected_job(app: &SpotifydlGuiApp, job_id: &JobId) -> bool {
    matches!(app.selected_job.as_ref(), Some(SelectedJob::Queue(selected)) if selected == job_id)
}

fn is_history_selected(app: &SpotifydlGuiApp, job_id: &JobId) -> bool {
    matches!(app.selected_job.as_ref(), Some(SelectedJob::History(selected)) if selected == job_id)
}

fn sync_settings_inputs(app: &mut SpotifydlGuiApp) {
    app.theme_mode = app.snapshot.settings.theme_mode;
    app.selected_backend = app.snapshot.settings.preferred_backend;
    app.external_backend_input = app.snapshot.settings.external_backend_executable.clone();
    app.default_destination_input = app.snapshot.settings.default_destination.clone();
    app.default_format_input = app.snapshot.settings.default_format.clone();
    app.max_parallel_input = app.snapshot.settings.max_parallel.to_string();
}

fn settings_dirty(app: &SpotifydlGuiApp) -> bool {
    app.selected_backend != app.snapshot.settings.preferred_backend
        || app.external_backend_input.trim() != app.snapshot.settings.external_backend_executable
        || app.default_destination_input.trim() != app.snapshot.settings.default_destination
        || app.default_format_input.trim() != app.snapshot.settings.default_format
        || app.max_parallel_input.trim() != app.snapshot.settings.max_parallel.to_string()
}

fn normalize_selected_job(app: &mut SpotifydlGuiApp) {
    let selected_is_valid = match &app.selected_job {
        Some(SelectedJob::Queue(job_id)) => {
            app.snapshot.queue.jobs.iter().any(|job| &job.id == job_id)
        }
        Some(SelectedJob::History(job_id)) => app
            .snapshot
            .history
            .iter()
            .any(|entry| &entry.job.id == job_id),
        None => false,
    };

    if selected_is_valid {
        return;
    }

    app.selected_job = app
        .snapshot
        .queue
        .active_job_id
        .as_ref()
        .cloned()
        .map(SelectedJob::Queue)
        .or_else(|| {
            app.snapshot
                .queue
                .jobs
                .first()
                .map(|job| SelectedJob::Queue(job.id.clone()))
        })
        .or_else(|| {
            app.snapshot
                .history
                .first()
                .map(|entry| SelectedJob::History(entry.job.id.clone()))
        });
}

fn selected_job_record<'a>(app: &'a SpotifydlGuiApp) -> Option<SelectedJobView<'a>> {
    match &app.selected_job {
        Some(SelectedJob::Queue(job_id)) => app
            .snapshot
            .queue
            .jobs
            .iter()
            .find(|job| &job.id == job_id)
            .map(|job| SelectedJobView::Queue { job }),
        Some(SelectedJob::History(job_id)) => app
            .snapshot
            .history
            .iter()
            .find(|entry| &entry.job.id == job_id)
            .map(|entry| SelectedJobView::History { entry }),
        None => None,
    }
}

fn selected_job_actions<'a>(
    app: &'a SpotifydlGuiApp,
    selected: &SelectedJobView<'a>,
) -> Element<'a, Message> {
    let job = selected.job();

    match selected {
        SelectedJobView::Queue { .. } => {
            let job_action = if app.snapshot.queue.active_job_id.as_ref() == Some(&job.id) {
                action_button(
                    "Cancel Active",
                    Message::CancelJob(job.id.clone()),
                    ButtonTone::Danger,
                )
            } else {
                action_button(
                    "Remove From Queue",
                    Message::RemoveQueueJob(job.id.clone()),
                    ButtonTone::Ghost,
                )
            };

            let row = if job_has_retryable_items(job) {
                row![
                    action_button(
                        "Retry Failed",
                        Message::RetryFailedItems(job.id.clone()),
                        ButtonTone::Secondary
                    ),
                    job_action,
                ]
            } else {
                row![job_action]
            };

            row.spacing(10).into()
        }
        SelectedJobView::History { .. } => row![
            action_button(
                "Requeue",
                Message::RequeueHistoryJob(job.id.clone()),
                ButtonTone::Secondary
            ),
            action_button(
                "Remove History",
                Message::RemoveHistoryJob(job.id.clone()),
                ButtonTone::Ghost
            ),
        ]
        .spacing(10)
        .into(),
    }
}

fn selected_job_outputs_panel(job: &JobRecord) -> Element<'_, Message> {
    let outputs = job
        .items
        .iter()
        .flat_map(|item| item.outputs.iter().map(move |output| (item, output)))
        .collect::<Vec<_>>();

    if outputs.is_empty() {
        return text("Files will appear here after the download finishes")
            .size(13)
            .into();
    }

    let content = outputs.into_iter().fold(
        column![text("Files").size(14)].spacing(6),
        |column, (item, output)| {
            column.push(
                container(
                    column![
                        text(format!("{} | {:?}", item.label, output.disposition)).size(13),
                        text(&output.final_path).size(13),
                        row![
                            action_button(
                                "Open Output",
                                Message::OpenPath(output.final_path.clone()),
                                ButtonTone::Secondary
                            ),
                            action_button(
                                "Show Folder",
                                Message::OpenContainingFolder(output.final_path.clone()),
                                ButtonTone::Ghost
                            ),
                        ]
                        .spacing(8),
                    ]
                    .spacing(4),
                )
                .style(soft_card_style)
                .padding(8)
                .width(Length::Fill),
            )
        },
    );

    container(content).width(Length::Fill).into()
}

fn selected_job_failures_panel(job: &JobRecord) -> Element<'_, Message> {
    let failed_items = job
        .items
        .iter()
        .filter_map(|item| item.failure_reason.as_ref().map(|reason| (item, reason)))
        .collect::<Vec<_>>();

    if failed_items.is_empty() {
        return text("No problems reported").size(13).into();
    }

    let content = failed_items.into_iter().fold(
        column![text("Problems").size(14)].spacing(6),
        |column, (item, reason)| {
            let code = reason.code.as_deref().unwrap_or("no code");
            let details = reason.details.as_deref().unwrap_or("no extra details");

            column.push(
                container(
                    column![
                        text(format!("{} | {:?}", item.label, reason.kind)).size(13),
                        text(format!("Message: {}", reason.message)).size(13),
                        text(format!("Code: {code}")).size(13),
                        text(format!("Details: {details}")).size(13),
                    ]
                    .spacing(3),
                )
                .style(soft_card_style)
                .padding(8)
                .width(Length::Fill),
            )
        },
    );

    container(content).width(Length::Fill).into()
}

fn job_timestamps_summary(job: &JobRecord) -> String {
    format!(
        "Created {} | Updated {} | Started {} | Finished {}",
        format_timestamp(job.created_at_ms),
        format_timestamp(job.updated_at_ms),
        job.started_at_ms
            .map(format_timestamp)
            .unwrap_or_else(|| "not started".to_string()),
        job.finished_at_ms
            .map(format_timestamp)
            .unwrap_or_else(|| "not finished".to_string()),
    )
}

fn format_job_log(entry: &LogEntry) -> String {
    format!(
        "{} {:?}/{:?} {}",
        format_timestamp(entry.timestamp_ms),
        entry.scope,
        entry.level,
        entry.message
    )
}

fn job_has_retryable_items(job: &JobRecord) -> bool {
    job.items
        .iter()
        .any(|item| item.state == ItemState::Failed || item.state == ItemState::Cancelled)
}

fn history_job_status_summary(snapshot: &AppSnapshot, job: &JobRecord) -> String {
    let mut highlights = vec![format!("{:?}", job.state).to_uppercase()];

    if snapshot
        .service_health
        .last_recovery_at_ms
        .is_some_and(|_| job.state == spotifydl_protocol::JobState::Paused)
    {
        highlights.push("RECOVERED AFTER RESTART".to_string());
    }

    append_job_health_highlights(job, &mut highlights);
    highlights.join(" | ")
}

fn append_job_health_highlights(job: &JobRecord, highlights: &mut Vec<String>) {
    if job_has_retryable_items(job) {
        highlights.push("RETRY AVAILABLE".to_string());
    }

    if job.totals.items_failed > 0 {
        highlights.push(format!("{} ITEM FAILED", job.totals.items_failed));
    }

    if job.totals.items_cancelled > 0 {
        highlights.push(format!("{} ITEM CANCELLED", job.totals.items_cancelled));
    }

    if job.totals.flagged_items > 0 {
        highlights.push(format!("{} FLAGGED", job.totals.flagged_items));
    }

    if let Some(error_message) = job.error_message.as_deref() {
        if !error_message.trim().is_empty() {
            highlights.push(format!("JOB ERROR: {error_message}"));
        }
    }
}

fn open_containing_folder(path: &str) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("No path available to open".to_string());
    }

    let parent = Path::new(trimmed)
        .parent()
        .ok_or_else(|| format!("Could not find a parent folder for `{trimmed}`"))?;

    open_path_os(parent)
}

fn open_path(path: &str) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("No path available to open".to_string());
    }

    open_path_os(Path::new(trimmed))
}

fn open_path_os(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.arg("/C").arg("start").arg("").arg(path);
        command
    };

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };

    command
        .spawn()
        .map_err(|error| format!("Failed to open `{}`: {error}", path.display()))?;

    Ok(())
}
