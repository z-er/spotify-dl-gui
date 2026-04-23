use std::path::PathBuf;

use spotifydl_protocol::{FailureReason, FailureReasonKind, JobState};
use thiserror::Error;

use crate::{BackendCapabilities, BackendEvent, BackendHealth, BackendRunRequest, DownloadBackend};

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

#[derive(Debug, Error)]
pub enum LibraryDownloaderError {
    #[error("library-backed spotify-dl integration is not implemented yet")]
    NotImplemented,
}

#[derive(Debug, Default)]
pub struct LibraryDownloader {
    config: LibraryDownloaderConfig,
}

impl LibraryDownloader {
    pub fn new(config: LibraryDownloaderConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &LibraryDownloaderConfig {
        &self.config
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
            notes: &[
                "Upstream already exposes a library crate via src/lib.rs; the CLI main.rs is a thin wrapper around library calls.",
                "Collection resolution currently expands album and playlist URLs into Vec<Track> before download starts.",
                "Track metadata includes track title, album, album artist, artist list, and position.",
                "Playlist title is not surfaced by track::get_tracks today; deeper integration may require an upstream extension or direct librespot playlist metadata fetch.",
            ],
        }
    }

    fn not_implemented_events(request: BackendRunRequest) -> Vec<BackendEvent> {
        vec![BackendEvent::JobFinished {
            job_id: request.job_id,
            state: JobState::Failed,
            failure_reason: Some(FailureReason {
                kind: FailureReasonKind::Unknown,
                code: Some("LIBRARY_ADAPTER_NOT_IMPLEMENTED".to_string()),
                message: "Library-backed spotify-dl integration is not implemented yet"
                    .to_string(),
                details: Some(
                    "See README-rust.md on rust-library-integration-plan for the current migration plan."
                        .to_string(),
                ),
            }),
        }]
    }
}

impl DownloadBackend for LibraryDownloader {
    fn name(&self) -> &'static str {
        "library-downloader"
    }

    fn health(&self) -> BackendHealth {
        BackendHealth {
            ready: false,
            message: "Library-backed spotify-dl integration is planned but not implemented"
                .to_string(),
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_immediate_pause_resume: false,
        }
    }

    fn start(&mut self, request: BackendRunRequest) -> Vec<BackendEvent> {
        Self::not_implemented_events(request)
    }

    fn control(&mut self, _request: crate::BackendControlRequest) -> Vec<BackendEvent> {
        Vec::new()
    }

    fn tick(&mut self) -> Vec<BackendEvent> {
        Vec::new()
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
}
