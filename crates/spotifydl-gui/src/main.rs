use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use chrono::{Local, TimeZone};
use directories::ProjectDirs;
use iced::{
    Alignment, Background, Color, Element, Length, Shadow, Subscription, Task, Theme, Vector,
    application, border, time,
    widget::{
        button, column, container, pick_list, progress_bar, row, scrollable, text, text_input,
        toggler,
    },
};
use rfd::FileDialog;
use spotifydl_protocol::{
    AppSnapshot, BackendKind, DownloadItem, HistoryEntry, ItemState, JobId, JobRecord, QueueStatus,
    ServiceCommand, ThemeMode,
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
    settings_tab: SettingsTab,
    show_history_screen: bool,
    show_details_screen: bool,
    show_settings_screen: bool,
    displayed_progress: HashMap<JobId, f32>,
    selected_job: Option<SelectedJob>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioFormatOption {
    value: &'static str,
    label: &'static str,
}

impl std::fmt::Display for AudioFormatOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedJob {
    Queue(JobId),
    History(JobId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Advanced,
}

#[derive(Debug, Clone, Copy)]
enum ChipTone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy)]
enum ButtonTone {
    Primary,
    Secondary,
    Ghost,
    Danger,
    Tab(bool),
}

#[derive(Debug, Clone)]
enum Message {
    UrlInputChanged(String),
    QueueUrls,
    DownloadUrls,
    StartQueue,
    PauseQueue,
    StopCurrent,
    CancelJob(JobId),
    RetryFailedItems(JobId),
    RemoveQueueJob(JobId),
    OpenPath(String),
    OpenContainingFolder(String),
    RequeueHistoryJob(JobId),
    RemoveHistoryJob(JobId),
    ClearHistory,
    OpenHistoryScreen,
    CloseHistoryScreen,
    SelectBackend(BackendKind),
    ExternalBackendInputChanged(String),
    DefaultDestinationInputChanged(String),
    SelectDefaultFormat(AudioFormatOption),
    MaxParallelInputChanged(String),
    BrowseDownloadFolder,
    ThemeModeToggled(bool),
    SelectSettingsTab(SettingsTab),
    SelectQueueJob(JobId),
    SelectHistoryJob(JobId),
    CloseDetailsScreen,
    OpenSettingsScreen,
    CloseSettingsScreen,
    ResetAdvancedSettings,
    SaveBackendSettings,
    Tick,
}

const AUDIO_FORMAT_OPTIONS: &[AudioFormatOption] = &[
    AudioFormatOption {
        value: "alac",
        label: "alac (caf)",
    },
    AudioFormatOption {
        value: "flac",
        label: "flac",
    },
    AudioFormatOption {
        value: "mp3",
        label: "mp3 (320 kbps)",
    },
    AudioFormatOption {
        value: "mp3-v0",
        label: "mp3 (V0)",
    },
    AudioFormatOption {
        value: "wav",
        label: "wav",
    },
];

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
        settings_tab: SettingsTab::General,
        show_history_screen: false,
        show_details_screen: false,
        show_settings_screen: false,
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
        Message::QueueUrls => {
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
        Message::DownloadUrls => {
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
                if !matches!(
                    app.snapshot.queue.status,
                    QueueStatus::Running | QueueStatus::Backoff
                ) {
                    dispatch(app, ServiceCommand::StartQueue);
                }
            }
        }
        Message::StartQueue => {
            dispatch(app, ServiceCommand::StartQueue);
        }
        Message::PauseQueue => {
            dispatch(app, ServiceCommand::PauseQueue);
        }
        Message::StopCurrent => {
            if let Some(job_id) = app.snapshot.queue.active_job_id.clone() {
                dispatch(app, ServiceCommand::CancelJob { job_id });
            }
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
        Message::OpenHistoryScreen => {
            app.show_history_screen = true;
        }
        Message::CloseHistoryScreen => {
            app.show_history_screen = false;
        }
        Message::CloseDetailsScreen => {
            app.show_details_screen = false;
        }
        Message::OpenSettingsScreen => {
            app.show_settings_screen = true;
        }
        Message::CloseSettingsScreen => {
            app.show_settings_screen = false;
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
        Message::SelectDefaultFormat(option) => {
            app.default_format_input = option.value.to_string();
        }
        Message::MaxParallelInputChanged(value) => {
            app.max_parallel_input = value;
        }
        Message::BrowseDownloadFolder => {
            if let Some(folder) = FileDialog::new().pick_folder() {
                app.default_destination_input = folder.display().to_string();
                app.last_error = None;
            }
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
        Message::SelectSettingsTab(tab) => {
            app.settings_tab = tab;
        }
        Message::SelectQueueJob(job_id) => {
            app.selected_job = Some(SelectedJob::Queue(job_id));
            app.show_details_screen = true;
        }
        Message::SelectHistoryJob(job_id) => {
            app.show_history_screen = false;
            app.selected_job = Some(SelectedJob::History(job_id));
            app.show_details_screen = true;
        }
        Message::ResetAdvancedSettings => {
            app.selected_backend = BackendKind::Library;
            app.external_backend_input.clear();
            app.max_parallel_input = "5".to_string();
            app.last_error = None;
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
    if app.show_history_screen {
        return history_screen(app);
    }
    if app.show_details_screen {
        return details_screen(app);
    }
    if app.show_settings_screen {
        return settings_screen(app);
    }

    let content = column![
        app_header(app),
        queue_controls(app),
        queue_list(app),
        bottom_control_bar(app),
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
    container(
        row![
            text_input("type a link here...", &app.url_input)
                .on_input(Message::UrlInputChanged)
                .on_submit(Message::QueueUrls)
                .padding([18, 20])
                .style(input_style)
                .size(28)
                .width(Length::Fill),
            action_button("queue", Message::QueueUrls, ButtonTone::Primary),
        ]
        .spacing(14)
        .align_y(Alignment::Center),
    )
    .style(flat_panel_style)
    .padding(16)
    .into()
}

fn bottom_control_bar(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let start_label = if app.snapshot.queue.status == QueueStatus::Paused {
        "resume q"
    } else {
        "start q"
    };
    let mut left = row![
        action_button(start_label, Message::StartQueue, ButtonTone::Secondary),
        action_button("pause q", Message::PauseQueue, ButtonTone::Ghost),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    if app.snapshot.queue.active_job_id.is_some() {
        left = left.push(action_button(
            "stop current",
            Message::StopCurrent,
            ButtonTone::Danger,
        ));
    }

    let right = row![
        action_button("history", Message::OpenHistoryScreen, ButtonTone::Ghost),
        action_button("settings", Message::OpenSettingsScreen, ButtonTone::Ghost),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    container(
        row![left.width(Length::Fill), right,]
            .align_y(Alignment::Center)
            .spacing(12),
    )
    .style(flat_panel_style)
    .padding(16)
    .into()
}

fn settings_tab_button<'a>(
    label: &'a str,
    tab: SettingsTab,
    selected: SettingsTab,
) -> Element<'a, Message> {
    let caption = if tab == selected {
        format!("[{label}]")
    } else {
        label.to_string()
    };

    action_button(
        caption,
        Message::SelectSettingsTab(tab),
        ButtonTone::Tab(tab == selected),
    )
}

fn app_header(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let mut header = column![text("spotify-dl-gui").size(54),]
        .spacing(4)
        .align_x(Alignment::Center);

    let status_text = match app.snapshot.queue.status {
        QueueStatus::Running => "downloading".to_string(),
        QueueStatus::Backoff => "waiting briefly".to_string(),
        QueueStatus::Paused => "paused".to_string(),
        QueueStatus::Idle if app.snapshot.queue.jobs.is_empty() => "ready".to_string(),
        QueueStatus::Idle => format!("{} queued", app.snapshot.queue.jobs.len()),
    };

    header = header.push(text(status_text).size(14));

    if let Some(error) = &app.last_error {
        header = header.push(text(format!("problem: {error}")).size(13));
    }

    container(header)
        .padding([6, 12])
        .width(Length::Fill)
        .into()
}

fn settings_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let settings_dirty = settings_dirty(app);
    let save_note = if settings_dirty {
        "You have unsaved changes"
    } else {
        "Saved settings are active"
    };
    let general_summary = format!(
        "Downloads go to `{}` as `{}`",
        blank_to_placeholder(
            &app.snapshot.settings.default_destination,
            "your default folder"
        ),
        blank_to_placeholder(&app.snapshot.settings.default_format, "your default format"),
    );
    let advanced_summary = format!(
        "Engine: {:?} | Parallel downloads: {}",
        app.snapshot.settings.preferred_backend, app.snapshot.settings.max_parallel
    );
    let active_change_note = if app.snapshot.queue.active_job_id.is_some() {
        "Saved changes apply to the next download. Anything already running keeps its current engine."
    } else {
        "Saved changes apply straight away while downloads are idle."
    };
    let helper_text = match app.selected_backend {
        BackendKind::External => {
            "Use this only if you want to point the app at a separate spotify-dl executable."
        }
        BackendKind::Library => "Recommended. Uses the built-in downloader integration.",
        BackendKind::Fake => "Testing only. This does not download real audio files.",
    };
    let settings_tabs = row![
        settings_tab_button("General", SettingsTab::General, app.settings_tab),
        settings_tab_button("Advanced", SettingsTab::Advanced, app.settings_tab),
    ]
    .spacing(8);

    let general_panel = {
        let folder_input = text_input(
            "Where downloads should be saved, e.g. C:\\Music",
            &app.default_destination_input,
        )
        .on_input(Message::DefaultDestinationInputChanged)
        .padding(12)
        .style(input_style)
        .width(Length::Fill);

        let folder_row = if app.default_destination_input.trim().is_empty() {
            row![
                folder_input,
                action_button("Browse", Message::BrowseDownloadFolder, ButtonTone::Ghost),
            ]
            .spacing(10)
        } else {
            row![
                folder_input,
                action_button("Browse", Message::BrowseDownloadFolder, ButtonTone::Ghost),
                action_button(
                    "Open Folder",
                    Message::OpenPath(app.default_destination_input.trim().to_string()),
                    ButtonTone::Ghost
                ),
            ]
            .spacing(10)
        };

        container(
            column![
                text("Appearance").size(15),
                row![
                    text("Light").size(13),
                    toggler(matches!(app.theme_mode, ThemeMode::Dark))
                        .on_toggle(Message::ThemeModeToggled),
                    text("Dark").size(13),
                ]
                .spacing(10),
                text("Download folder").size(15),
                folder_row,
                text("Audio format").size(15),
                pick_list(
                    AUDIO_FORMAT_OPTIONS,
                    selected_audio_format(app),
                    Message::SelectDefaultFormat,
                )
                .padding([10, 12])
                .width(Length::Fill),
                text("Available right now: alac (caf), flac, mp3 (320 kbps), mp3 (V0), and wav.")
                    .size(13),
                text("alac is written in a CAF container for Apple-friendly lossless playback. opus is still not exposed because it would add packaging complexity without a good music-focused encoder path yet.")
                    .size(13),
                text(save_note).size(13),
                text(general_summary).size(13),
                action_button("Save", Message::SaveBackendSettings, ButtonTone::Primary),
            ]
            .spacing(12),
        )
        .style(soft_card_style)
        .padding(16)
    };

    let advanced_panel = {
        let backend_row = row![
            backend_button("Library", BackendKind::Library, app.selected_backend),
            backend_button("External", BackendKind::External, app.selected_backend),
            backend_button("Fake", BackendKind::Fake, app.selected_backend),
        ]
        .spacing(10);

        container(
            column![
                text("Download engine").size(15),
                text(format!(
                    "Using: {}",
                    app.snapshot.service_health.backend_name
                ))
                .size(14),
                text(format!("Status: {}", format_backend_status(&app.snapshot))).size(13),
                backend_row,
                text("External app override").size(15),
                text_input(
                    "Leave blank unless you want to point to a separate spotify-dl executable",
                    &app.external_backend_input
                )
                .on_input(Message::ExternalBackendInputChanged)
                .padding(12)
                .style(input_style)
                .width(Length::Fill),
                text("Parallel downloads").size(15),
                text_input(
                    "Recommended: 1 to 5 tracks at once",
                    &app.max_parallel_input
                )
                .on_input(Message::MaxParallelInputChanged)
                .padding(12)
                .style(input_style)
                .width(Length::Fill),
                text(save_note).size(13),
                text(advanced_summary).size(13),
                text("Higher numbers usually do not make downloads finish faster and can cause more failures.").size(13),
                text(helper_text).size(13),
                text(active_change_note).size(13),
                row![
                    action_button(
                        "Reset Recommended",
                        Message::ResetAdvancedSettings,
                        ButtonTone::Ghost
                    ),
                    action_button("Save", Message::SaveBackendSettings, ButtonTone::Primary),
                ]
                .spacing(10),
            ]
            .spacing(12),
        )
        .style(soft_card_style)
        .padding(16)
    };

    let panel = match app.settings_tab {
        SettingsTab::General => general_panel,
        SettingsTab::Advanced => advanced_panel,
    };

    column![text("Settings").size(24), settings_tabs, panel,]
        .spacing(12)
        .into()
}

fn settings_screen(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let content = column![
        app_header(app),
        container(
            column![
                row![
                    text("Settings").size(24),
                    action_button("Close", Message::CloseSettingsScreen, ButtonTone::Ghost),
                ]
                .spacing(12),
                scrollable(settings_panel(app)).height(Length::Fill),
            ]
            .spacing(12),
        )
        .style(card_style)
        .padding(16),
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
    let completed_history = app
        .snapshot
        .history
        .iter()
        .filter(|entry| entry.job.state == spotifydl_protocol::JobState::Completed)
        .take(12)
        .collect::<Vec<_>>();

    if jobs.is_empty() && completed_history.is_empty() {
        return container(text("No downloads yet. Queue a link above to get started.").size(18))
            .style(flat_panel_style)
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }

    let active_job_id = app.snapshot.queue.active_job_id.as_ref();
    let mut items = jobs.iter().fold(column!().spacing(12), |column, job| {
        column.push(queue_job_row(app, job, active_job_id == Some(&job.id)))
    });

    if !completed_history.is_empty() {
        items = items.push(text("Completed").size(16));
        items = completed_history
            .into_iter()
            .fold(items, |column, entry| column.push(history_job_row(entry)));
    }

    scrollable(items).height(Length::Fill).into()
}

fn queue_job_row<'a>(
    app: &'a SpotifydlGuiApp,
    job: &'a JobRecord,
    is_active: bool,
) -> Element<'a, Message> {
    let displayed_progress = app
        .displayed_progress
        .get(&job.id)
        .copied()
        .unwrap_or(f32::from(job.progress.percent));
    let status_line = queue_card_status_text(&app.snapshot, job, displayed_progress);
    let issue_summary = queue_issue_summary(&app.snapshot, job);
    let action = queue_job_action(job, issue_summary.as_deref());

    let mut progress_column = column![
        text(&job.label).size(20),
        progress_bar(0.0..=100.0, displayed_progress)
            .height(12)
            .style(progress_style)
            .width(Length::Fill),
        text(status_line).size(14),
    ]
    .spacing(8)
    .width(Length::Fill);

    if let Some(issue_summary) = issue_summary {
        progress_column = progress_column.push(text(issue_summary).size(13));
    }

    container(
        row![cover_tile(job), progress_column, action,]
            .spacing(16)
            .align_y(Alignment::Center),
    )
    .style(move |theme| job_row_style(theme, is_active))
    .padding(16)
    .width(Length::Fill)
    .into()
}

fn history_job_row<'a>(entry: &'a HistoryEntry) -> Element<'a, Message> {
    let job = &entry.job;
    let action = completed_job_actions(job);

    container(
        row![
            cover_tile(job),
            column![
                text(&job.label).size(20),
                progress_bar(0.0..=100.0, 100.0)
                    .height(12)
                    .style(progress_style)
                    .width(Length::Fill),
                text("done").size(14),
            ]
            .spacing(8)
            .width(Length::Fill),
            action,
        ]
        .spacing(16)
        .align_y(Alignment::Center),
    )
    .style(move |theme| job_row_style(theme, false))
    .padding(16)
    .width(Length::Fill)
    .into()
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
                action_button("Close", Message::CloseHistoryScreen, ButtonTone::Ghost),
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

fn history_screen(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let content = column![app_header(app), history_panel(app),]
        .spacing(20)
        .padding(24)
        .height(Length::Fill);

    container(content)
        .style(app_shell_style)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn details_screen(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let content = column![
        app_header(app),
        container(
            column![
                row![
                    text("Details").size(24),
                    action_button("Close", Message::CloseDetailsScreen, ButtonTone::Ghost),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
                selected_job_panel(app),
            ]
            .spacing(12),
        )
        .style(flat_panel_style)
        .padding(16),
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

fn selected_job_panel(app: &SpotifydlGuiApp) -> Element<'_, Message> {
    let Some(selected) = selected_job_record(app) else {
        return container(
            column![
                text("No download selected").size(24),
                text("Pick a job from history or open a problem job to inspect its details.")
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

    let files_panel = if has_job_outputs(job) {
        Some(
            container(column![text("Files").size(15), selected_job_outputs_panel(job),].spacing(8))
                .style(soft_card_style)
                .padding(14)
                .width(Length::Fill),
        )
    } else {
        None
    };

    let problems_panel = if has_job_problems(job) {
        Some(
            container(
                column![text("Problems").size(15), selected_job_failures_panel(job),].spacing(8),
            )
            .style(soft_card_style)
            .padding(14)
            .width(Length::Fill),
        )
    } else {
        None
    };

    let mut content = column![
        text("Download details").size(24),
        text(format!("{section_label} | {:?}", job.state)).size(14),
        text(&job.label).size(18),
        selected_job_actions(app, &selected),
        container(
            column![
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
                job_items_panel(&job.items),
            ]
            .spacing(10),
        )
        .style(soft_card_style)
        .padding(14)
        .width(Length::Fill),
    ]
    .spacing(10);

    if let Some(files_panel) = files_panel {
        content = content.push(files_panel);
    }
    if let Some(problems_panel) = problems_panel {
        content = content.push(problems_panel);
    }

    container(content)
        .style(soft_card_style)
        .padding(16)
        .width(Length::Fill)
        .into()
}

fn queue_card_status_text(
    snapshot: &AppSnapshot,
    job: &JobRecord,
    displayed_progress: f32,
) -> String {
    if snapshot.queue.active_job_id.as_ref() == Some(&job.id)
        && snapshot.queue.status == QueueStatus::Backoff
    {
        return "waiting briefly".to_string();
    }

    match job.state {
        spotifydl_protocol::JobState::Queued => "queued".to_string(),
        spotifydl_protocol::JobState::Paused => {
            if let Some(current_item) = queue_current_item_text(job) {
                format!("paused - {current_item}")
            } else {
                "paused".to_string()
            }
        }
        spotifydl_protocol::JobState::Running => {
            let progress = displayed_progress.round() as u16;
            if let Some(stage) = queue_progress_stage(snapshot, job) {
                format!("{progress}% - {}", stage.to_ascii_lowercase())
            } else {
                format!("{progress}% - downloading")
            }
        }
        spotifydl_protocol::JobState::Completed => "done".to_string(),
        spotifydl_protocol::JobState::Failed => "needs attention".to_string(),
        spotifydl_protocol::JobState::Cancelled => "cancelled".to_string(),
    }
}

fn queue_job_action(job: &JobRecord, issue_summary: Option<&str>) -> Element<'static, Message> {
    if let Some(path) = first_output_path(job) {
        if job.state == spotifydl_protocol::JobState::Completed {
            return completed_action_row(path);
        }
    }

    if issue_summary.is_some()
        || matches!(
            job.state,
            spotifydl_protocol::JobState::Failed | spotifydl_protocol::JobState::Cancelled
        )
    {
        return action_button(
            "details",
            Message::SelectQueueJob(job.id.clone()),
            ButtonTone::Ghost,
        );
    }

    action_button(
        "cancel",
        Message::CancelJob(job.id.clone()),
        ButtonTone::Ghost,
    )
}

fn completed_job_actions(job: &JobRecord) -> Element<'static, Message> {
    if let Some(path) = first_output_path(job) {
        return completed_action_row(path);
    }

    action_button(
        "details",
        Message::SelectHistoryJob(job.id.clone()),
        ButtonTone::Ghost,
    )
}

fn completed_action_row(path: String) -> Element<'static, Message> {
    row![
        action_button(
            "open",
            Message::OpenPath(path.clone()),
            ButtonTone::Secondary
        ),
        action_button(
            "folder",
            Message::OpenContainingFolder(path),
            ButtonTone::Ghost
        ),
    ]
    .spacing(8)
    .into()
}

fn first_output_path(job: &JobRecord) -> Option<String> {
    job.items
        .iter()
        .flat_map(|item| item.outputs.iter())
        .map(|output| output.final_path.clone())
        .next()
}

fn cover_tile(job: &JobRecord) -> Element<'_, Message> {
    let initials = label_initials(&job.label);

    container(text(initials).size(26))
        .style(cover_tile_style)
        .width(72)
        .height(72)
        .center_x(72)
        .center_y(72)
        .into()
}

fn label_initials(label: &str) -> String {
    let mut pieces = label
        .split(|character: char| !character.is_alphanumeric())
        .filter(|piece| !piece.is_empty())
        .take(2)
        .map(|piece| piece.chars().next().unwrap_or('D').to_ascii_uppercase())
        .collect::<String>();

    if pieces.is_empty() {
        pieces.push_str("DL");
    }

    pieces
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

fn has_job_outputs(job: &JobRecord) -> bool {
    job.items.iter().any(|item| !item.outputs.is_empty())
}

fn has_job_problems(job: &JobRecord) -> bool {
    job.error_message
        .as_deref()
        .is_some_and(|message| !message.trim().is_empty())
        || job.items.iter().any(|item| item.failure_reason.is_some())
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
            Self::Queue { job } => &job.original_inputs,
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

fn selected_audio_format(app: &SpotifydlGuiApp) -> Option<AudioFormatOption> {
    AUDIO_FORMAT_OPTIONS
        .iter()
        .find(|option| option.value == app.default_format_input.trim())
        .copied()
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

fn queue_progress_stage(snapshot: &AppSnapshot, job: &JobRecord) -> Option<String> {
    if snapshot.queue.active_job_id.as_ref() == Some(&job.id)
        && snapshot.queue.status == QueueStatus::Backoff
    {
        return Some("Waiting briefly".to_string());
    }

    if !matches!(
        job.state,
        spotifydl_protocol::JobState::Running | spotifydl_protocol::JobState::Paused
    ) {
        return None;
    }

    let stage = compact_progress_stage(&job.progress.detail);
    if stage.is_empty() { None } else { Some(stage) }
}

fn queue_transition_state(snapshot: &AppSnapshot, job: &JobRecord) -> Option<(String, ChipTone)> {
    let is_active = snapshot.queue.active_job_id.as_ref() == Some(&job.id);

    if is_active && snapshot.queue.status == QueueStatus::Backoff {
        return Some(("Cooling down".to_string(), ChipTone::Warning));
    }

    if let Some(message) = recent_job_control_message(snapshot, &job.id) {
        let lower = message.to_ascii_lowercase();
        if lower.contains("cancelling library downloader") {
            return Some(("Cancelling…".to_string(), ChipTone::Danger));
        }
        if lower.contains("pausing library downloader") {
            return Some(("Pausing…".to_string(), ChipTone::Warning));
        }
        if lower.contains("resuming library downloader") {
            return Some(("Resuming…".to_string(), ChipTone::Accent));
        }
        if lower.contains("backend will pause after the active job reaches a safe boundary") {
            return Some(("Finishing current step".to_string(), ChipTone::Warning));
        }
    }

    if job.state == spotifydl_protocol::JobState::Paused {
        return Some(("Paused".to_string(), ChipTone::Neutral));
    }

    if is_active && job.state == spotifydl_protocol::JobState::Running {
        return Some(("Active".to_string(), ChipTone::Success));
    }

    None
}

fn queue_current_item_text(job: &JobRecord) -> Option<String> {
    let item = job
        .items
        .iter()
        .find(|item| item.state == spotifydl_protocol::ItemState::Running)
        .or_else(|| {
            job.items
                .iter()
                .find(|item| item.state == spotifydl_protocol::ItemState::Paused)
        })?;

    let prefix = if item.state == spotifydl_protocol::ItemState::Paused {
        "Paused on"
    } else {
        "Now on"
    };

    Some(format!("{prefix}: {}", item.label))
}

fn recent_job_control_message(
    snapshot: &AppSnapshot,
    job_id: &spotifydl_protocol::JobId,
) -> Option<String> {
    let now = current_time_ms();
    snapshot
        .logs
        .iter()
        .rev()
        .find(|entry| {
            entry.job_id.as_ref() == Some(job_id)
                && now.saturating_sub(entry.timestamp_ms) <= 6_000
                && (entry.message.contains("library downloader")
                    || entry.message.contains("safe boundary"))
        })
        .map(|entry| entry.message.clone())
}

fn compact_progress_stage(detail: &str) -> String {
    let head = detail
        .split('|')
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();

    match head.as_str() {
        "" => String::new(),
        "downloading" => "Downloading".to_string(),
        "encoding" => "Encoding".to_string(),
        "writing" => "Writing".to_string(),
        "tagging" => "Tagging".to_string(),
        "processing" => "Processing".to_string(),
        "backend started item" | "waiting for backend events" | "waiting for downloader" => {
            "Preparing".to_string()
        }
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        }
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

fn current_time_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
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

fn flat_panel_style(theme: &Theme) -> iced::widget::container::Style {
    if theme_is_dark(theme) {
        let base = theme.palette();

        return iced::widget::container::Style::default()
            .background(surface_color(theme, 0.07))
            .border(
                border::rounded(12)
                    .width(1)
                    .color(with_alpha(blend(base.primary, Color::WHITE, 0.08), 0.14)),
            );
    }

    let palette = theme.extended_palette();

    iced::widget::container::Style::default()
        .background(palette.background.base.color)
        .border(
            border::rounded(12)
                .width(1)
                .color(with_alpha(palette.background.strong.color, 0.18)),
        )
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

fn job_row_style(theme: &Theme, is_active: bool) -> iced::widget::container::Style {
    let base_style = flat_panel_style(theme);
    let palette = theme.extended_palette();

    if is_active {
        return base_style.border(
            border::rounded(12)
                .width(1)
                .color(with_alpha(palette.primary.strong.color, 0.38)),
        );
    }

    base_style
}

fn cover_tile_style(theme: &Theme) -> iced::widget::container::Style {
    let palette = theme.extended_palette();
    let background = if theme_is_dark(theme) {
        with_alpha(Color::WHITE, 0.10)
    } else {
        palette.background.strong.color
    };

    iced::widget::container::Style::default()
        .background(background)
        .color(palette.background.base.text)
        .border(
            border::rounded(8)
                .width(1)
                .color(with_alpha(palette.background.strong.color, 0.25)),
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
        border: border::rounded(8),
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
        border: border::rounded(10).width(1).color(border_color),
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
