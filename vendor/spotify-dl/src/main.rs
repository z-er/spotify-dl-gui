use std::time::Duration;

use spotify_dl::download::{DownloadOptions, Downloader, RateLimitConfig};
use spotify_dl::encoder::Format;
use spotify_dl::log;
use spotify_dl::session::create_session;
use spotify_dl::track::get_tracks;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "spotify-dl",
    about = "A commandline utility to download music directly from Spotify"
)]
struct Opt {
    #[structopt(
        help = "A list of Spotify URIs or URLs (songs, podcasts, playlists or albums)",
        required = true
    )]
    tracks: Vec<String>,
    #[structopt(
        short = "d",
        long = "destination",
        help = "The directory where the songs will be downloaded"
    )]
    destination: Option<String>,
    #[structopt(
        short = "t",
        long = "parallel",
        help = "Number of parallel downloads. Default is 5.",
        default_value = "5"
    )]
    parallel: usize,
    #[structopt(
        short = "f",
        long = "format",
        help = "The format to download the tracks in. Default is flac.",
        default_value = "flac"
    )]
    format: Format,
    #[structopt(
        short = "F",
        long = "force",
        help = "Force download even if the file already exists"
    )]
    force: bool,
    #[structopt(
        long = "failure-delay-ms",
        help = "Base delay in milliseconds to wait after a download fails",
        default_value = "0"
    )]
    failure_delay_ms: u64,
    #[structopt(
        long = "failure-delay-multiplier",
        help = "Multiplier applied to the delay for consecutive failures",
        default_value = "2.0"
    )]
    failure_delay_multiplier: f64,
    #[structopt(
        long = "failure-delay-max-ms",
        help = "Maximum delay in milliseconds when backing off after failures",
        default_value = "60000"
    )]
    failure_delay_max_ms: u64,
    #[structopt(
        long = "json-events",
        help = "Emit machine-readable JSON events alongside normal output"
    )]
    json_events: bool,
}

pub fn create_destination_if_required(destination: Option<String>) -> anyhow::Result<()> {
    if let Some(destination) = destination {
        if !std::path::Path::new(&destination).exists() {
            tracing::info!("Creating destination directory: {}", destination);
            std::fs::create_dir_all(destination)?;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    log::configure_logger()?;

    let opt = Opt::from_args();
    create_destination_if_required(opt.destination.clone())?;

    if opt.tracks.is_empty() {
        eprintln!("No tracks provided");
        std::process::exit(1);
    }

    let session = create_session().await?;

    let track = get_tracks(opt.tracks, &session).await?;

    let mut download_options =
        DownloadOptions::new(opt.destination, opt.parallel, opt.format, opt.force);
    let rate_limit = RateLimitConfig::new(
        Duration::from_millis(opt.failure_delay_ms),
        opt.failure_delay_multiplier,
        Duration::from_millis(opt.failure_delay_max_ms),
    );
    download_options.set_rate_limit(rate_limit);
    download_options.enable_json_events(opt.json_events);

    let downloader = Downloader::new(session);
    downloader.download_tracks(track, &download_options).await
}
