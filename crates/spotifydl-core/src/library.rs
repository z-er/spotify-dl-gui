use spotifydl_protocol::{FailureReason, FailureReasonKind, ItemMetadata, JobState};
use std::path::PathBuf;
use thiserror::Error;

use crate::{
    BackendCapabilities, BackendControl, BackendControlRequest, BackendEvent, BackendHealth,
    BackendRunRequest, DownloadBackend,
};

#[cfg(feature = "vendored-upstream")]
use crate::BackendItemDescriptor;
#[cfg(feature = "vendored-upstream")]
use spotifydl_protocol::{
    BackoffState, IntegrityFlag, IntegritySeverity, ItemState, JobId, OutputDisposition,
    OutputRecord, ProgressState,
};
#[cfg(feature = "vendored-upstream")]
use std::collections::HashMap;
#[cfg(feature = "vendored-upstream")]
use std::sync::Arc;
#[cfg(feature = "vendored-upstream")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "vendored-upstream")]
use std::sync::mpsc::{self, Receiver, TryRecvError};
#[cfg(feature = "vendored-upstream")]
use std::thread;
#[cfg(feature = "vendored-upstream")]
use tokio::runtime::Builder;

#[cfg(feature = "vendored-upstream")]
const DISCOVERY_NOTES: &[&str] = &[
    "Upstream already exposes a library crate via src/lib.rs; the CLI main.rs is a thin wrapper around library calls.",
    "Collection resolution currently expands album and playlist URLs into Vec<Track> before download starts.",
    "Track metadata includes track title, album, album artist, artist list, and position.",
    "Playlist title is not surfaced by track::get_tracks today; deeper integration may require an upstream extension or direct librespot playlist metadata fetch.",
    "This build links the vendored upstream crate via the spotifydl-core `vendored-upstream` feature.",
    "The vendored upstream snapshot is pinned under vendor/spotify-dl at commit f71baa6537ecc59a0dafe45c7dc74e0c8965f488.",
];

#[cfg(not(feature = "vendored-upstream"))]
const DISCOVERY_NOTES: &[&str] = &[
    "Upstream already exposes a library crate via src/lib.rs; the CLI main.rs is a thin wrapper around library calls.",
    "Collection resolution currently expands album and playlist URLs into Vec<Track> before download starts.",
    "Track metadata includes track title, album, album artist, artist list, and position.",
    "Playlist title is not surfaced by track::get_tracks today; deeper integration may require an upstream extension or direct librespot playlist metadata fetch.",
    "The vendored upstream snapshot lives under vendor/spotify-dl but this build did not enable the spotifydl-core `vendored-upstream` feature.",
];

#[derive(Debug, Clone, Default)]
pub struct LibraryDownloaderConfig {
    pub upstream_checkout: Option<PathBuf>,
    pub credentials_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamIntegrationSurface {
    SessionCreation,
    CollectionResolution,
    TrackMetadata,
    DownloadOrchestration,
    RateLimitBackoff,
}

#[derive(Debug, Clone)]
pub struct UpstreamDiscovery {
    pub crate_name: &'static str,
    pub public_modules: &'static [&'static str],
    pub surfaces: &'static [UpstreamIntegrationSurface],
    pub session_entrypoint: &'static str,
    pub collection_entrypoint: &'static str,
    pub download_entrypoint: &'static str,
    pub notes: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedCollectionKind {
    Track,
    Album,
    Playlist,
    Episode,
    Mixed,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ResolvedCollectionItem {
    pub position: u32,
    pub url: String,
    pub label: String,
    pub metadata: ItemMetadata,
}

#[derive(Debug, Clone)]
pub struct ResolvedCollection {
    pub source_url: String,
    pub kind: ResolvedCollectionKind,
    pub label: String,
    pub items: Vec<ResolvedCollectionItem>,
}

#[derive(Debug, Error)]
pub enum LibraryDownloaderError {
    #[error("vendored upstream feature is not enabled")]
    FeatureDisabled,
    #[error("download cancelled")]
    Cancelled,
    #[error("failed to initialize async runtime: {0}")]
    RuntimeInitialization(String),
    #[error("failed to parse requested output format: {0}")]
    UnsupportedFormat(String),
    #[error("failed to create upstream session: {0}")]
    SessionCreate(String),
    #[error("failed to resolve collection from source: {0}")]
    TrackResolution(String),
    #[error("failed to fetch metadata for item #{item_index}: {message}")]
    MetadataFetch { item_index: usize, message: String },
}

#[cfg(feature = "vendored-upstream")]
#[derive(Debug)]
enum LibraryWorkerMessage {
    DownloadEvent(upstream_spotify_dl::download::DownloadEvent),
    Finished(Result<(), LibraryDownloaderError>),
}

#[cfg(feature = "vendored-upstream")]
#[derive(Debug)]
struct ActiveLibraryJob {
    request: BackendRunRequest,
    output_rx: Receiver<LibraryWorkerMessage>,
    saw_item_failure: bool,
    cancel_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct LibraryDownloader {
    config: LibraryDownloaderConfig,
    #[cfg(feature = "vendored-upstream")]
    active_jobs: HashMap<JobId, ActiveLibraryJob>,
}

impl LibraryDownloader {
    pub fn new(config: LibraryDownloaderConfig) -> Self {
        Self {
            config,
            #[cfg(feature = "vendored-upstream")]
            active_jobs: HashMap::new(),
        }
    }

    pub fn config(&self) -> &LibraryDownloaderConfig {
        &self.config
    }

    pub fn vendored_upstream_enabled() -> bool {
        cfg!(feature = "vendored-upstream")
    }

    pub fn resolve_collection(
        &self,
        source_url: &str,
    ) -> Result<ResolvedCollection, LibraryDownloaderError> {
        #[cfg(feature = "vendored-upstream")]
        {
            self.resolve_collection_vendored(source_url)
        }

        #[cfg(not(feature = "vendored-upstream"))]
        {
            let _ = source_url;
            Err(LibraryDownloaderError::FeatureDisabled)
        }
    }

    pub fn discovery() -> UpstreamDiscovery {
        UpstreamDiscovery {
            crate_name: "spotify-dl",
            public_modules: &["download", "encoder", "log", "session", "stream", "track"],
            surfaces: &[
                UpstreamIntegrationSurface::SessionCreation,
                UpstreamIntegrationSurface::CollectionResolution,
                UpstreamIntegrationSurface::TrackMetadata,
                UpstreamIntegrationSurface::DownloadOrchestration,
                UpstreamIntegrationSurface::RateLimitBackoff,
            ],
            session_entrypoint: "spotify_dl::session::create_session",
            collection_entrypoint: "spotify_dl::track::get_tracks",
            download_entrypoint: "spotify_dl::download::Downloader::download_tracks",
            notes: DISCOVERY_NOTES,
        }
    }

    #[cfg(feature = "vendored-upstream")]
    fn resolve_collection_vendored(
        &self,
        source_url: &str,
    ) -> Result<ResolvedCollection, LibraryDownloaderError> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| LibraryDownloaderError::RuntimeInitialization(error.to_string()))?;

        runtime.block_on(self.resolve_collection_async(source_url))
    }

    #[cfg(feature = "vendored-upstream")]
    async fn resolve_collection_async(
        &self,
        source_url: &str,
    ) -> Result<ResolvedCollection, LibraryDownloaderError> {
        let session = upstream_spotify_dl::session::create_session()
            .await
            .map_err(|error| LibraryDownloaderError::SessionCreate(error.to_string()))?;

        let tracks = upstream_spotify_dl::track::get_tracks(vec![source_url.to_string()], &session)
            .await
            .map_err(|error| LibraryDownloaderError::TrackResolution(error.to_string()))?;

        if tracks.is_empty() {
            return Err(LibraryDownloaderError::TrackResolution(
                "upstream did not resolve any supported tracks".to_string(),
            ));
        }

        let kind = infer_collection_kind(source_url, tracks.len());
        let mut items = Vec::with_capacity(tracks.len());

        for (index, track) in tracks.into_iter().enumerate() {
            let track_metadata = track.metadata(&session).await.map_err(|error| {
                LibraryDownloaderError::MetadataFetch {
                    item_index: index + 1,
                    message: error.to_string(),
                }
            })?;

            let artists = track_metadata
                .artists
                .iter()
                .map(|artist| artist.name.clone())
                .collect::<Vec<_>>();

            let metadata = ItemMetadata {
                artist: artists.join(", "),
                album: track_metadata.album.name.clone(),
                title: track_metadata.track_name.clone(),
                album_artist: artists.first().cloned().unwrap_or_default(),
            };

            let position = track_metadata
                .position
                .or(track.position)
                .unwrap_or(index + 1) as u32;

            let url = track.id.to_uri().unwrap_or_else(|_| source_url.to_string());

            items.push(ResolvedCollectionItem {
                position,
                url,
                label: item_label(&metadata),
                metadata,
            });
        }

        Ok(ResolvedCollection {
            source_url: source_url.to_string(),
            kind,
            label: collection_label(source_url, kind, &items),
            items,
        })
    }

    #[cfg(feature = "vendored-upstream")]
    fn spawn_library_job(
        &self,
        request: BackendRunRequest,
    ) -> Result<
        (
            Receiver<LibraryWorkerMessage>,
            Arc<AtomicBool>,
            Arc<AtomicBool>,
        ),
        LibraryDownloaderError,
    > {
        let (tx, rx) = mpsc::channel();
        let config = self.config.clone();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let pause_flag = Arc::new(AtomicBool::new(false));
        let worker_cancel_flag = Arc::clone(&cancel_flag);
        let worker_pause_flag = Arc::clone(&pause_flag);

        thread::spawn(move || {
            let completion = run_library_job(
                request,
                config,
                worker_cancel_flag,
                worker_pause_flag,
                tx.clone(),
            );
            let _ = tx.send(LibraryWorkerMessage::Finished(completion));
        });

        Ok((rx, cancel_flag, pause_flag))
    }

    #[cfg(feature = "vendored-upstream")]
    fn translate_download_event(
        request: &BackendRunRequest,
        event: upstream_spotify_dl::download::DownloadEvent,
    ) -> (Vec<BackendEvent>, bool) {
        let item = match &event {
            upstream_spotify_dl::download::DownloadEvent::TrackStart {
                track_id,
                track_label,
            }
            | upstream_spotify_dl::download::DownloadEvent::TrackFailed {
                track_id,
                track_label,
                ..
            }
            | upstream_spotify_dl::download::DownloadEvent::TrackSkipped {
                track_id,
                track_label,
            }
            | upstream_spotify_dl::download::DownloadEvent::TrackComplete {
                track_id,
                track_label,
                ..
            }
            | upstream_spotify_dl::download::DownloadEvent::Retry {
                track_id,
                track_label,
                ..
            }
            | upstream_spotify_dl::download::DownloadEvent::RateLimitBackoff {
                track_id,
                track_label,
                ..
            }
            | upstream_spotify_dl::download::DownloadEvent::RateLimitWait {
                track_id,
                track_label,
                ..
            }
            | upstream_spotify_dl::download::DownloadEvent::Stage {
                track_id,
                track_label,
                ..
            } => resolve_request_item(request, Some(track_id.as_str()), Some(track_label.as_str())),
        };
        let item_id = item.map(|value| value.item_id.clone());

        match event {
            upstream_spotify_dl::download::DownloadEvent::TrackStart { track_label, .. } => (
                item_id
                    .map(|item_id| {
                        vec![BackendEvent::ItemStarted {
                            job_id: request.job_id.clone(),
                            item_id,
                            label: Some(track_label),
                            metadata: None,
                        }]
                    })
                    .unwrap_or_else(|| unmatched_library_event_log(request, "track_start")),
                false,
            ),
            upstream_spotify_dl::download::DownloadEvent::Stage {
                stage,
                status,
                progress,
                ..
            } => (
                item_id
                    .map(|item_id| {
                        vec![BackendEvent::ItemProgress {
                            job_id: request.job_id.clone(),
                            item_id,
                            progress: ProgressState::from_steps(
                                progress.round() as u32,
                                100,
                                format!("{stage}: {status}"),
                            ),
                        }]
                    })
                    .unwrap_or_else(|| unmatched_library_event_log(request, "stage")),
                false,
            ),
            upstream_spotify_dl::download::DownloadEvent::Retry {
                stage,
                attempt,
                max_attempts,
                ..
            } => (
                vec![BackendEvent::Log {
                    job_id: Some(request.job_id.clone()),
                    item_id,
                    message: format!(
                        "Library downloader retrying {stage}, attempt {attempt}/{max_attempts}"
                    ),
                }],
                false,
            ),
            upstream_spotify_dl::download::DownloadEvent::TrackFailed { reason, .. } => (
                item_id
                    .map(|item_id| {
                        vec![BackendEvent::ItemFinished {
                            job_id: request.job_id.clone(),
                            item_id,
                            state: ItemState::Failed,
                            failure_reason: Some(FailureReason {
                                kind: FailureReasonKind::Unknown,
                                code: Some("LIBRARY_TRACK_FAILED".to_string()),
                                message: reason,
                                details: None,
                            }),
                        }]
                    })
                    .unwrap_or_else(|| unmatched_library_event_log(request, "track_failed")),
                true,
            ),
            upstream_spotify_dl::download::DownloadEvent::TrackSkipped { .. } => (
                item_id
                    .map(|item_id| {
                        vec![
                            BackendEvent::ItemFlag {
                                job_id: request.job_id.clone(),
                                item_id: item_id.clone(),
                                flag: IntegrityFlag {
                                    severity: IntegritySeverity::Info,
                                    kind: "skip".to_string(),
                                    message: "Library downloader skipped existing output"
                                        .to_string(),
                                    details: None,
                                },
                            },
                            BackendEvent::ItemFinished {
                                job_id: request.job_id.clone(),
                                item_id,
                                state: ItemState::Completed,
                                failure_reason: None,
                            },
                        ]
                    })
                    .unwrap_or_else(|| unmatched_library_event_log(request, "track_skipped")),
                false,
            ),
            upstream_spotify_dl::download::DownloadEvent::TrackComplete {
                track_label,
                path,
                ..
            } => (
                item_id
                    .map(|item_id| {
                        vec![
                            BackendEvent::ItemOutput {
                                job_id: request.job_id.clone(),
                                item_id: item_id.clone(),
                                output: OutputRecord {
                                    disposition: OutputDisposition::Written,
                                    final_path: path.clone(),
                                    artist: item
                                        .map(|value| value.metadata.artist.clone())
                                        .unwrap_or_default(),
                                    album: item
                                        .map(|value| value.metadata.album.clone())
                                        .unwrap_or_default(),
                                    title: item
                                        .map(|value| value.metadata.title.clone())
                                        .filter(|value| !value.is_empty())
                                        .unwrap_or(track_label),
                                    size_bytes: 0,
                                    format: PathBuf::from(&path)
                                        .extension()
                                        .and_then(|value| value.to_str())
                                        .unwrap_or_default()
                                        .to_string(),
                                    details: Some(
                                        "Reported by vendored library downloader".to_string(),
                                    ),
                                },
                            },
                            BackendEvent::ItemFinished {
                                job_id: request.job_id.clone(),
                                item_id,
                                state: ItemState::Completed,
                                failure_reason: None,
                            },
                        ]
                    })
                    .unwrap_or_else(|| unmatched_library_event_log(request, "track_complete")),
                false,
            ),
            upstream_spotify_dl::download::DownloadEvent::RateLimitBackoff {
                delay_ms,
                reason,
                ..
            } => (
                vec![BackendEvent::BackoffStarted {
                    job_id: Some(request.job_id.clone()),
                    state: BackoffState {
                        active: true,
                        reason,
                        delay_ms,
                        remaining_ms: delay_ms,
                    },
                }],
                false,
            ),
            upstream_spotify_dl::download::DownloadEvent::RateLimitWait { waited_ms, .. } => (
                vec![BackendEvent::BackoffTick {
                    job_id: Some(request.job_id.clone()),
                    state: BackoffState {
                        active: false,
                        reason: "Library downloader wait finished".to_string(),
                        delay_ms: waited_ms,
                        remaining_ms: 0,
                    },
                }],
                false,
            ),
        }
    }
}

#[cfg(any(feature = "vendored-upstream", test))]
fn infer_collection_kind(source_url: &str, item_count: usize) -> ResolvedCollectionKind {
    let source_url = source_url.to_ascii_lowercase();

    if source_url.contains("/album/") {
        ResolvedCollectionKind::Album
    } else if source_url.contains("/playlist/") {
        ResolvedCollectionKind::Playlist
    } else if source_url.contains("/episode/") {
        ResolvedCollectionKind::Episode
    } else if source_url.contains("/track/") || item_count == 1 {
        ResolvedCollectionKind::Track
    } else if item_count > 1 {
        ResolvedCollectionKind::Mixed
    } else {
        ResolvedCollectionKind::Unknown
    }
}

#[cfg(any(feature = "vendored-upstream", test))]
fn item_label(metadata: &ItemMetadata) -> String {
    let title = metadata.title.trim();
    let artist = metadata.artist.trim();

    if title.is_empty() && artist.is_empty() {
        "Untitled item".to_string()
    } else if artist.is_empty() {
        title.to_string()
    } else if title.is_empty() {
        artist.to_string()
    } else {
        format!("{title} - {artist}")
    }
}

#[cfg(any(feature = "vendored-upstream", test))]
fn collection_label(
    source_url: &str,
    kind: ResolvedCollectionKind,
    items: &[ResolvedCollectionItem],
) -> String {
    match kind {
        ResolvedCollectionKind::Album => {
            if let Some((album, artist)) = common_album_label(items) {
                if artist.is_empty() {
                    album
                } else {
                    format!("{album} - {artist}")
                }
            } else {
                format!("Album download ({})", items.len())
            }
        }
        ResolvedCollectionKind::Playlist => "Playlist download".to_string(),
        ResolvedCollectionKind::Track | ResolvedCollectionKind::Episode => items
            .first()
            .map(|item| item.label.clone())
            .unwrap_or_else(|| fallback_source_label(source_url)),
        ResolvedCollectionKind::Mixed => format!("Collection download ({})", items.len()),
        ResolvedCollectionKind::Unknown => fallback_source_label(source_url),
    }
}

#[cfg(any(feature = "vendored-upstream", test))]
fn common_album_label(items: &[ResolvedCollectionItem]) -> Option<(String, String)> {
    let first = items.first()?;
    let album = first.metadata.album.trim();

    if album.is_empty()
        || items.iter().any(|item| {
            item.metadata.album.trim() != album || item.metadata.album.trim().is_empty()
        })
    {
        return None;
    }

    let artist = items
        .iter()
        .find_map(|item| {
            let album_artist = item.metadata.album_artist.trim();
            if !album_artist.is_empty() {
                Some(album_artist.to_string())
            } else {
                let artist = item.metadata.artist.trim();
                (!artist.is_empty()).then(|| artist.to_string())
            }
        })
        .unwrap_or_default();

    Some((album.to_string(), artist))
}

#[cfg(any(feature = "vendored-upstream", test))]
fn fallback_source_label(source_url: &str) -> String {
    let trimmed = source_url.trim();
    if trimmed.is_empty() {
        "Untitled source".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(feature = "vendored-upstream")]
fn run_library_job(
    request: BackendRunRequest,
    _config: LibraryDownloaderConfig,
    cancel_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<LibraryWorkerMessage>,
) -> Result<(), LibraryDownloaderError> {
    let (event_tx, event_rx) = mpsc::channel::<upstream_spotify_dl::download::DownloadEvent>();
    let forward_tx = tx.clone();
    thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            let _ = forward_tx.send(LibraryWorkerMessage::DownloadEvent(event));
        }
    });

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| LibraryDownloaderError::RuntimeInitialization(error.to_string()))?;

    runtime.block_on(async move {
        let session = upstream_spotify_dl::session::create_session()
            .await
            .map_err(|error| LibraryDownloaderError::SessionCreate(error.to_string()))?;

        let tracks = upstream_spotify_dl::track::get_tracks(
            request.items.iter().map(|item| item.url.clone()).collect(),
            &session,
        )
        .await
        .map_err(|error| LibraryDownloaderError::TrackResolution(error.to_string()))?;

        let format = request
            .options
            .format
            .parse::<upstream_spotify_dl::encoder::Format>()
            .map_err(|_| {
                LibraryDownloaderError::UnsupportedFormat(request.options.format.clone())
            })?;

        if !request.options.destination.trim().is_empty() {
            std::fs::create_dir_all(&request.options.destination).map_err(|error| {
                LibraryDownloaderError::TrackResolution(format!(
                    "failed to create destination directory: {error}"
                ))
            })?;
        }

        let mut options = upstream_spotify_dl::download::DownloadOptions::new(
            Some(request.options.destination.clone()),
            request.options.max_parallel.max(1) as usize,
            format,
            request
                .options
                .extra_args
                .iter()
                .any(|arg| arg == "--force" || arg == "-F"),
        );
        options.set_event_sender(event_tx);
        options.set_cancel_flag(cancel_flag);
        options.set_pause_flag(pause_flag);
        options.enable_json_events(false);

        let downloader = upstream_spotify_dl::download::Downloader::new(session);
        downloader
            .download_tracks(tracks, &options)
            .await
            .map_err(|error| {
                let message = error.to_string();
                if message.eq_ignore_ascii_case("download cancelled") {
                    LibraryDownloaderError::Cancelled
                } else {
                    LibraryDownloaderError::TrackResolution(message)
                }
            })
    })
}

#[cfg(feature = "vendored-upstream")]
fn resolve_request_item<'a>(
    request: &'a BackendRunRequest,
    track_id: Option<&str>,
    track_label: Option<&str>,
) -> Option<&'a BackendItemDescriptor> {
    if request.items.len() == 1 {
        return request.items.first();
    }

    if let Some(track_id) = track_id {
        if let Some(item) = request
            .items
            .iter()
            .find(|item| spotify_identity(&item.url).as_deref() == Some(track_id))
        {
            return Some(item);
        }
    }

    if let Some(track_label) = track_label {
        if let Some(item) = request.items.iter().find(|item| item.label == track_label) {
            return Some(item);
        }
    }

    None
}

#[cfg(feature = "vendored-upstream")]
fn unmatched_library_event_log(request: &BackendRunRequest, kind: &str) -> Vec<BackendEvent> {
    vec![BackendEvent::Log {
        job_id: Some(request.job_id.clone()),
        item_id: None,
        message: format!("Unmatched library event `{kind}`"),
    }]
}

#[cfg(feature = "vendored-upstream")]
fn spotify_identity(url: &str) -> Option<String> {
    if let Some(value) = url.strip_prefix("spotify:track:") {
        return Some(value.to_string());
    }
    if let Some(value) = url.strip_prefix("spotify:episode:") {
        return Some(value.to_string());
    }

    for marker in ["/track/", "/episode/"] {
        if let Some(index) = url.find(marker) {
            return url[index + marker.len()..]
                .split(&['?', '/', '&'][..])
                .next()
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
        }
    }

    None
}

impl DownloadBackend for LibraryDownloader {
    fn name(&self) -> &'static str {
        "library-downloader"
    }

    fn health(&self) -> BackendHealth {
        if Self::vendored_upstream_enabled() {
            BackendHealth {
                ready: true,
                message: "Ready (vendored spotify-dl library linked)".to_string(),
            }
        } else {
            BackendHealth {
                ready: false,
                message: "Library-backed spotify-dl integration is scaffolded; enable spotifydl-core feature `vendored-upstream` to link the vendored crate"
                    .to_string(),
            }
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_immediate_pause_resume: true,
        }
    }

    fn validate_run_request(&self, request: &BackendRunRequest) -> Result<(), String> {
        if request.items.is_empty() {
            return Err("library downloader request did not include any items".to_string());
        }

        #[cfg(feature = "vendored-upstream")]
        {
            request
                .options
                .format
                .parse::<upstream_spotify_dl::encoder::Format>()
                .map(|_| ())
                .map_err(|_| format!("unsupported output format `{}`", request.options.format))
        }

        #[cfg(not(feature = "vendored-upstream"))]
        {
            let _ = request;
            Err("vendored upstream feature is not enabled".to_string())
        }
    }

    fn start(&mut self, request: BackendRunRequest) -> Vec<BackendEvent> {
        #[cfg(feature = "vendored-upstream")]
        {
            match self.spawn_library_job(request.clone()) {
                Ok((output_rx, cancel_flag, pause_flag)) => {
                    let job_id = request.job_id.clone();
                    self.active_jobs.insert(
                        job_id.clone(),
                        ActiveLibraryJob {
                            request,
                            output_rx,
                            saw_item_failure: false,
                            cancel_flag,
                            pause_flag,
                        },
                    );

                    vec![
                        BackendEvent::Log {
                            job_id: Some(job_id.clone()),
                            item_id: None,
                            message: "Launching vendored library downloader".to_string(),
                        },
                        BackendEvent::JobStarted { job_id },
                    ]
                }
                Err(error) => vec![
                    BackendEvent::Log {
                        job_id: Some(request.job_id.clone()),
                        item_id: None,
                        message: format!("Failed to launch library downloader: {error}"),
                    },
                    BackendEvent::JobFinished {
                        job_id: request.job_id,
                        state: JobState::Failed,
                        failure_reason: Some(FailureReason {
                            kind: FailureReasonKind::Io,
                            code: Some("LIBRARY_BACKEND_START".to_string()),
                            message: error.to_string(),
                            details: None,
                        }),
                    },
                ],
            }
        }

        #[cfg(not(feature = "vendored-upstream"))]
        {
            vec![BackendEvent::JobFinished {
                job_id: request.job_id,
                state: JobState::Failed,
                failure_reason: Some(FailureReason {
                    kind: FailureReasonKind::Unknown,
                    code: Some("LIBRARY_ADAPTER_NOT_IMPLEMENTED".to_string()),
                    message: "Library-backed spotify-dl integration is not available"
                        .to_string(),
                    details: Some(
                        "Enable the spotifydl-core `vendored-upstream` feature to use the library downloader."
                            .to_string(),
                    ),
                }),
            }]
        }
    }

    fn control(&mut self, request: BackendControlRequest) -> Vec<BackendEvent> {
        match request.control {
            BackendControl::Pause => {
                #[cfg(feature = "vendored-upstream")]
                {
                    if let Some(job) = self.active_jobs.get(&request.job_id) {
                        job.pause_flag.store(true, Ordering::SeqCst);
                        return vec![BackendEvent::Log {
                            job_id: Some(request.job_id),
                            item_id: None,
                            message: "Pausing library downloader at the next safe boundary"
                                .to_string(),
                        }];
                    }
                }

                vec![BackendEvent::Log {
                    job_id: Some(request.job_id),
                    item_id: None,
                    message: "Pause requested, but no active library job was found".to_string(),
                }]
            }
            BackendControl::Resume => {
                #[cfg(feature = "vendored-upstream")]
                {
                    if let Some(job) = self.active_jobs.get(&request.job_id) {
                        job.pause_flag.store(false, Ordering::SeqCst);
                        return vec![BackendEvent::Log {
                            job_id: Some(request.job_id),
                            item_id: None,
                            message: "Resuming library downloader".to_string(),
                        }];
                    }
                }

                vec![BackendEvent::Log {
                    job_id: Some(request.job_id),
                    item_id: None,
                    message: "Resume requested, but no active library job was found".to_string(),
                }]
            }
            BackendControl::Cancel => {
                #[cfg(feature = "vendored-upstream")]
                {
                    if let Some(job) = self.active_jobs.get(&request.job_id) {
                        job.cancel_flag.store(true, Ordering::SeqCst);
                        return vec![BackendEvent::Log {
                            job_id: Some(request.job_id),
                            item_id: None,
                            message: "Cancelling library downloader at the next safe boundary"
                                .to_string(),
                        }];
                    }
                }

                vec![BackendEvent::Log {
                    job_id: Some(request.job_id),
                    item_id: None,
                    message: "Cancel requested, but no active library job was found".to_string(),
                }]
            }
        }
    }

    fn tick(&mut self) -> Vec<BackendEvent> {
        #[cfg(feature = "vendored-upstream")]
        {
            let mut completed = Vec::new();
            let mut events = Vec::new();

            for (job_id, job) in &mut self.active_jobs {
                loop {
                    match job.output_rx.try_recv() {
                        Ok(LibraryWorkerMessage::DownloadEvent(event)) => {
                            let (mut mapped, saw_failure) =
                                Self::translate_download_event(&job.request, event);
                            if saw_failure {
                                job.saw_item_failure = true;
                            }
                            events.append(&mut mapped);
                        }
                        Ok(LibraryWorkerMessage::Finished(result)) => {
                            let (state, failure_reason) =
                                classify_library_job_result(result, job.saw_item_failure);
                            events.push(BackendEvent::JobFinished {
                                job_id: job_id.clone(),
                                state,
                                failure_reason,
                            });
                            completed.push(job_id.clone());
                            break;
                        }
                        Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                    }
                }
            }

            for job_id in completed {
                self.active_jobs.remove(&job_id);
            }

            events
        }

        #[cfg(not(feature = "vendored-upstream"))]
        {
            Vec::new()
        }
    }
}

#[cfg(feature = "vendored-upstream")]
fn classify_library_job_result(
    result: Result<(), LibraryDownloaderError>,
    saw_item_failure: bool,
) -> (JobState, Option<FailureReason>) {
    match result {
        Ok(()) if !saw_item_failure => (JobState::Completed, None),
        Ok(()) => (
            JobState::Failed,
            Some(FailureReason {
                kind: FailureReasonKind::Unknown,
                code: Some("ITEM_FAILURES_REPORTED".to_string()),
                message: "Library downloader reported one or more item failures".to_string(),
                details: None,
            }),
        ),
        Err(LibraryDownloaderError::Cancelled) => (
            JobState::Cancelled,
            Some(FailureReason {
                kind: FailureReasonKind::Cancelled,
                code: Some("LIBRARY_BACKEND_CANCELLED".to_string()),
                message: "Cancelled by user".to_string(),
                details: Some(
                    "Vendored library downloader stopped after a cancellation request".to_string(),
                ),
            }),
        ),
        Err(error) => (
            JobState::Failed,
            Some(FailureReason {
                kind: FailureReasonKind::Unknown,
                code: Some("LIBRARY_BACKEND_FAILED".to_string()),
                message: error.to_string(),
                details: None,
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_expected_upstream_discovery_surface() {
        let discovery = LibraryDownloader::discovery();

        assert_eq!(discovery.crate_name, "spotify-dl");
        assert_eq!(
            discovery.session_entrypoint,
            "spotify_dl::session::create_session"
        );
        assert_eq!(
            discovery.collection_entrypoint,
            "spotify_dl::track::get_tracks"
        );
        assert_eq!(
            discovery.download_entrypoint,
            "spotify_dl::download::Downloader::download_tracks"
        );
        assert!(
            discovery
                .surfaces
                .contains(&UpstreamIntegrationSurface::CollectionResolution)
        );
    }

    #[test]
    fn derives_track_item_and_collection_labels_from_metadata() {
        let metadata = ItemMetadata {
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            title: "Song".to_string(),
            album_artist: "Artist".to_string(),
        };

        let item = ResolvedCollectionItem {
            position: 1,
            url: "spotify:track:123".to_string(),
            label: item_label(&metadata),
            metadata,
        };

        assert_eq!(item.label, "Song - Artist");
        assert_eq!(
            collection_label(
                "https://open.spotify.com/track/123",
                ResolvedCollectionKind::Track,
                &[item]
            ),
            "Song - Artist"
        );
    }

    #[test]
    fn derives_album_collection_label_from_common_metadata() {
        let first = ResolvedCollectionItem {
            position: 1,
            url: "spotify:track:1".to_string(),
            label: "Song 1 - Artist".to_string(),
            metadata: ItemMetadata {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                title: "Song 1".to_string(),
                album_artist: "Artist".to_string(),
            },
        };
        let second = ResolvedCollectionItem {
            position: 2,
            url: "spotify:track:2".to_string(),
            label: "Song 2 - Artist".to_string(),
            metadata: ItemMetadata {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                title: "Song 2".to_string(),
                album_artist: "Artist".to_string(),
            },
        };

        assert_eq!(
            collection_label(
                "https://open.spotify.com/album/123",
                ResolvedCollectionKind::Album,
                &[first, second]
            ),
            "Album - Artist"
        );
    }

    #[test]
    fn infers_collection_kind_from_source_url() {
        assert_eq!(
            infer_collection_kind("https://open.spotify.com/album/123", 10),
            ResolvedCollectionKind::Album
        );
        assert_eq!(
            infer_collection_kind("https://open.spotify.com/playlist/123", 10),
            ResolvedCollectionKind::Playlist
        );
        assert_eq!(
            infer_collection_kind("https://open.spotify.com/track/123", 1),
            ResolvedCollectionKind::Track
        );
    }

    #[cfg(feature = "vendored-upstream")]
    #[test]
    fn vendored_upstream_symbols_are_linkable() {
        let _ = upstream_spotify_dl::session::create_session;
        let _ = upstream_spotify_dl::track::get_tracks;
        let _ = std::any::type_name::<upstream_spotify_dl::download::Downloader>();
    }

    #[cfg(feature = "vendored-upstream")]
    #[test]
    fn classifies_cancelled_library_result_as_cancelled_job() {
        let (state, reason) =
            classify_library_job_result(Err(LibraryDownloaderError::Cancelled), false);

        assert_eq!(state, JobState::Cancelled);
        let reason = reason.expect("cancelled job should include a failure reason");
        assert_eq!(reason.kind, FailureReasonKind::Cancelled);
        assert_eq!(reason.code.as_deref(), Some("LIBRARY_BACKEND_CANCELLED"));
    }

    #[cfg(feature = "vendored-upstream")]
    #[test]
    fn cancel_control_sets_active_job_cancellation_flag() {
        let job_id = JobId("job-cancel".to_string());
        let request = BackendRunRequest {
            job_id: job_id.clone(),
            label: "Test".to_string(),
            source_url: "https://open.spotify.com/track/test".to_string(),
            options: spotifydl_protocol::JobOptions::default(),
            items: vec![BackendItemDescriptor {
                item_id: spotifydl_protocol::ItemId("item-1".to_string()),
                position: 1,
                label: "Item".to_string(),
                url: "spotify:track:test".to_string(),
                metadata: ItemMetadata::default(),
            }],
        };
        let (_tx, rx) = mpsc::channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let pause_flag = Arc::new(AtomicBool::new(false));
        let mut downloader = LibraryDownloader::default();
        downloader.active_jobs.insert(
            job_id.clone(),
            ActiveLibraryJob {
                request,
                output_rx: rx,
                saw_item_failure: false,
                cancel_flag: Arc::clone(&cancel_flag),
                pause_flag,
            },
        );

        let events = downloader.control(BackendControlRequest {
            job_id,
            control: BackendControl::Cancel,
        });

        assert!(cancel_flag.load(Ordering::SeqCst));
        assert!(matches!(
            events.as_slice(),
            [BackendEvent::Log { message, .. }] if message.contains("Cancelling library downloader")
        ));
    }

    #[cfg(feature = "vendored-upstream")]
    #[test]
    fn pause_and_resume_controls_toggle_pause_flag() {
        let job_id = JobId("job-pause".to_string());
        let request = BackendRunRequest {
            job_id: job_id.clone(),
            label: "Pause Test".to_string(),
            source_url: "https://open.spotify.com/track/test".to_string(),
            options: spotifydl_protocol::JobOptions::default(),
            items: vec![BackendItemDescriptor {
                item_id: spotifydl_protocol::ItemId("item-1".to_string()),
                position: 1,
                label: "Item".to_string(),
                url: "spotify:track:test".to_string(),
                metadata: ItemMetadata::default(),
            }],
        };
        let (_tx, rx) = mpsc::channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let pause_flag = Arc::new(AtomicBool::new(false));
        let mut downloader = LibraryDownloader::default();
        downloader.active_jobs.insert(
            job_id.clone(),
            ActiveLibraryJob {
                request,
                output_rx: rx,
                saw_item_failure: false,
                cancel_flag,
                pause_flag: Arc::clone(&pause_flag),
            },
        );

        let pause_events = downloader.control(BackendControlRequest {
            job_id: job_id.clone(),
            control: BackendControl::Pause,
        });
        assert!(pause_flag.load(Ordering::SeqCst));
        assert!(matches!(
            pause_events.as_slice(),
            [BackendEvent::Log { message, .. }] if message.contains("Pausing library downloader")
        ));

        let resume_events = downloader.control(BackendControlRequest {
            job_id,
            control: BackendControl::Resume,
        });
        assert!(!pause_flag.load(Ordering::SeqCst));
        assert!(matches!(
            resume_events.as_slice(),
            [BackendEvent::Log { message, .. }] if message.contains("Resuming library downloader")
        ));
    }
}
