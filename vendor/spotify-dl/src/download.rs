use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::StreamExt;
use futures::TryStreamExt;
use indicatif::MultiProgress;
use indicatif::ProgressBar;
use indicatif::ProgressState;
use indicatif::ProgressStyle;
use librespot::core::session::Session;

use crate::encoder;
use crate::encoder::Format;
use crate::encoder::Samples;
use crate::stream::Stream;
use crate::stream::StreamEvent;
use crate::stream::StreamEventChannel;
use crate::track::Track;
use crate::track::TrackMetadata;

use tokio::sync::Mutex;
use tokio::time::sleep;

pub struct Downloader {
    session: Session,
    progress_bar: MultiProgress,
}

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub base_delay: Duration,
    pub multiplier: f64,
    pub max_delay: Duration,
    pub reset_after_success: bool,
}

impl RateLimitConfig {
    pub fn new(base_delay: Duration, multiplier: f64, max_delay: Duration) -> Self {
        let multiplier = if multiplier.is_finite() {
            multiplier
        } else {
            1.0
        };
        let multiplier = multiplier.max(1.0);
        let max_delay = if max_delay < base_delay {
            base_delay
        } else {
            max_delay
        };
        RateLimitConfig {
            base_delay,
            multiplier,
            max_delay,
            reset_after_success: true,
        }
    }

    pub fn disabled() -> Self {
        RateLimitConfig {
            base_delay: Duration::ZERO,
            multiplier: 1.0,
            max_delay: Duration::ZERO,
            reset_after_success: true,
        }
    }

    pub fn is_enabled(&self) -> bool {
        !self.base_delay.is_zero()
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig::disabled()
    }
}

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub destination: PathBuf,
    pub parallel: usize,
    pub format: Format,
    pub force: bool,
    pub rate_limit: RateLimitConfig,
    pub json_events: bool,
}

impl DownloadOptions {
    pub fn new(destination: Option<String>, parallel: usize, format: Format, force: bool) -> Self {
        let destination =
            destination.map_or_else(|| std::env::current_dir().unwrap(), PathBuf::from);
        DownloadOptions {
            destination,
            parallel,
            format,
            force,
            rate_limit: RateLimitConfig::default(),
            json_events: false,
        }
    }

    pub fn set_rate_limit(&mut self, rate_limit: RateLimitConfig) {
        self.rate_limit = rate_limit;
    }

    pub fn enable_json_events(&mut self, enabled: bool) {
        self.json_events = enabled;
    }
}

#[derive(Clone)]
struct RateLimiter {
    config: RateLimitConfig,
    state: Arc<Mutex<RateLimiterState>>,
}

impl RateLimiter {
    fn new(config: RateLimitConfig) -> Self {
        RateLimiter {
            config,
            state: Arc::new(Mutex::new(RateLimiterState::default())),
        }
    }

    async fn wait_ready(&self) -> Option<Duration> {
        if !self.config.is_enabled() {
            return None;
        }

        let mut waited = Duration::ZERO;

        loop {
            let sleep_duration = {
                let mut state = self.state.lock().await;
                if let Some(next_ready) = state.next_ready {
                    let now = Instant::now();
                    if next_ready > now {
                        Some(next_ready - now)
                    } else {
                        state.next_ready = None;
                        None
                    }
                } else {
                    None
                }
            };

            match sleep_duration {
                Some(duration) if !duration.is_zero() => {
                    sleep(duration).await;
                    waited += duration;
                }
                Some(_) => continue,
                None => break,
            }
        }

        if waited.is_zero() { None } else { Some(waited) }
    }

    async fn on_failure(&self) -> Duration {
        if !self.config.is_enabled() {
            return Duration::ZERO;
        }

        let mut state = self.state.lock().await;
        let now = Instant::now();

        let next_delay = if state.current_delay.is_zero() {
            self.config.base_delay
        } else {
            let scaled = state.current_delay.as_secs_f64() * self.config.multiplier.max(1.0);
            let clamped = scaled.max(self.config.base_delay.as_secs_f64()).min(
                self.config
                    .max_delay
                    .max(self.config.base_delay)
                    .as_secs_f64(),
            );
            Duration::from_secs_f64(clamped)
        };

        if next_delay.is_zero() {
            state.current_delay = Duration::ZERO;
            state.next_ready = None;
            return Duration::ZERO;
        }

        state.current_delay = next_delay;
        let proposed_ready = now + next_delay;
        state.next_ready = match state.next_ready {
            Some(existing) if existing > proposed_ready => Some(existing),
            _ => Some(proposed_ready),
        };

        next_delay
    }

    async fn on_success(&self) {
        if !self.config.is_enabled() {
            return;
        }

        if !self.config.reset_after_success {
            return;
        }

        let mut state = self.state.lock().await;
        let now = Instant::now();
        if matches!(state.next_ready, Some(next) if next > now) {
            // Keep the pending sleep to let other tasks honor it.
            return;
        }

        state.current_delay = Duration::ZERO;
        state.next_ready = None;
    }
}

#[derive(Default)]
struct RateLimiterState {
    current_delay: Duration,
    next_ready: Option<Instant>,
}

#[derive(Debug, Clone)]
struct EventEmitter {
    enabled: bool,
}

impl EventEmitter {
    fn new(enabled: bool) -> Self {
        EventEmitter { enabled }
    }

    fn emit(&self, kind: &str, fields: &[EventField<'_>]) {
        if !self.enabled {
            return;
        }

        let mut output = String::with_capacity(128);
        output.push('{');
        output.push_str("\"event\":\"");
        output.push_str(&json_escape(kind));
        output.push('"');
        for field in fields {
            output.push(',');
            output.push('"');
            output.push_str(&json_escape(field.key));
            output.push_str("\":");
            field.value.write_json(&mut output);
        }
        output.push('}');
        println!("{}", output);
    }

    fn emit_track_event(
        &self,
        kind: &str,
        track_id: &str,
        track_label: &str,
        extra: &[EventField<'_>],
    ) {
        if !self.enabled {
            return;
        }
        let mut fields = Vec::with_capacity(extra.len() + 2);
        fields.push(EventField::str("track_id", track_id));
        fields.push(EventField::str("track", track_label));
        fields.extend_from_slice(extra);
        self.emit(kind, &fields);
    }

    fn emit_stage(
        &self,
        stage: &str,
        status: &str,
        progress: f64,
        track_id: &str,
        track_label: &str,
    ) {
        let progress = progress.clamp(0.0, 100.0);
        let fields = [
            EventField::str("stage", stage),
            EventField::str("status", status),
            EventField::float("progress", progress),
        ];
        self.emit_track_event("stage", track_id, track_label, &fields);
    }

    fn emit_retry(
        &self,
        track_id: &str,
        track_label: &str,
        stage: &str,
        attempt: usize,
        max_attempts: usize,
    ) {
        let fields = [
            EventField::str("stage", stage),
            EventField::int("attempt", attempt as i64),
            EventField::int("max_attempts", max_attempts as i64),
        ];
        self.emit_track_event("retry", track_id, track_label, &fields);
    }

    fn emit_failure(&self, track_id: &str, track_label: &str, reason: &str) {
        let fields = [EventField::str("reason", reason)];
        self.emit_track_event("track_failed", track_id, track_label, &fields);
    }

    fn emit_skip(&self, track_id: &str, track_label: &str) {
        self.emit_track_event("track_skipped", track_id, track_label, &[]);
    }

    fn emit_start(&self, track_id: &str, track_label: &str) {
        self.emit_track_event("track_start", track_id, track_label, &[]);
    }

    fn emit_complete(&self, track_id: &str, track_label: &str, path: &str) {
        let fields = [EventField::str("path", path)];
        self.emit_track_event("track_complete", track_id, track_label, &fields);
    }

    fn emit_backoff(&self, track_id: &str, track_label: &str, delay_ms: u64, reason: &str) {
        let fields = [
            EventField::int("delay_ms", delay_ms as i64),
            EventField::str("reason", reason),
        ];
        self.emit_track_event("rate_limit_backoff", track_id, track_label, &fields);
    }

    fn emit_wait(&self, track_id: &str, track_label: &str, waited_ms: u64) {
        let fields = [EventField::int("waited_ms", waited_ms as i64)];
        self.emit_track_event("rate_limit_wait", track_id, track_label, &fields);
    }
}

#[derive(Clone, Copy)]
struct EventField<'a> {
    key: &'a str,
    value: EventValue<'a>,
}

impl<'a> EventField<'a> {
    fn new(key: &'a str, value: EventValue<'a>) -> Self {
        EventField { key, value }
    }

    fn str(key: &'a str, value: &'a str) -> Self {
        EventField::new(key, EventValue::Str(value))
    }

    fn int(key: &'a str, value: i64) -> Self {
        EventField::new(key, EventValue::Int(value))
    }

    fn float(key: &'a str, value: f64) -> Self {
        EventField::new(key, EventValue::Float(value))
    }

    #[allow(dead_code)]
    fn bool(key: &'a str, value: bool) -> Self {
        EventField::new(key, EventValue::Bool(value))
    }
}

#[derive(Clone, Copy)]
enum EventValue<'a> {
    Str(&'a str),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl EventValue<'_> {
    fn write_json(self, buf: &mut String) {
        match self {
            EventValue::Str(value) => {
                buf.push('"');
                buf.push_str(&json_escape(value));
                buf.push('"');
            }
            EventValue::Int(value) => {
                let _ = write!(buf, "{}", value);
            }
            EventValue::Float(value) => {
                if value.is_finite() {
                    let _ = write!(buf, "{:.2}", value);
                } else {
                    buf.push_str("null");
                }
            }
            EventValue::Bool(value) => {
                buf.push_str(if value { "true" } else { "false" });
            }
        }
    }
}

fn json_escape(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped
}

impl Downloader {
    pub fn new(session: Session) -> Self {
        Downloader {
            session,
            progress_bar: MultiProgress::new(),
        }
    }

    pub async fn download_tracks(
        &self,
        tracks: Vec<Track>,
        options: &DownloadOptions,
    ) -> Result<()> {
        let rate_limiter = Arc::new(RateLimiter::new(options.rate_limit.clone()));
        let event_emitter = Arc::new(EventEmitter::new(options.json_events));

        futures::stream::iter(tracks)
            .map(|track| {
                let rate_limiter = Arc::clone(&rate_limiter);
                let event_emitter = Arc::clone(&event_emitter);
                async move {
                    self.download_track(track, options, rate_limiter, event_emitter)
                        .await
                }
            })
            .buffer_unordered(options.parallel)
            .try_collect::<Vec<_>>()
            .await?;

        Ok(())
    }

    #[tracing::instrument(
        name = "download_track",
        skip(self, options, rate_limiter, event_emitter)
    )]
    async fn download_track(
        &self,
        track: Track,
        options: &DownloadOptions,
        rate_limiter: Arc<RateLimiter>,
        event_emitter: Arc<EventEmitter>,
    ) -> Result<()> {
        let track_id = track.id.to_id().unwrap_or_else(|_| track.id.to_string());

        let waited = rate_limiter.wait_ready().await;

        let metadata = track.metadata(&self.session).await?;
        let track_label = metadata.to_string();
        if let Some(waited) = waited {
            if waited > Duration::ZERO {
                event_emitter.emit_wait(&track_id, &track_label, waited.as_millis() as u64);
            }
        }
        tracing::info!("Downloading track: {:?}", metadata.track_name);

        let filename = format!("{}.{}", track_label, options.format.extension());
        let path = options
            .destination
            .join(&filename)
            .to_str()
            .ok_or(anyhow::anyhow!("Could not set the output path"))?
            .to_string();

        if !options.force && PathBuf::from(&path).exists() {
            tracing::info!(
                "Skipping {}, file already exists. Use --force to force re-downloading the track",
                &metadata.track_name
            );
            rate_limiter.on_success().await;
            event_emitter.emit_skip(&track_id, &track_label);
            return Ok(());
        }

        event_emitter.emit_start(&track_id, &track_label);

        let pb = self.add_progress_bar(&metadata);

        event_emitter.emit_stage("downloading", "start", 0.0, &track_id, &track_label);

        let stream = Stream::new(self.session.clone());
        let channel = match stream.stream(track).await {
            Ok(channel) => channel,
            Err(e) => {
                let reason = e.to_string();
                self.fail_with_error(&pb, &track_label, reason.clone());
                event_emitter.emit_failure(&track_id, &track_label, &reason);
                self.backoff_after_failure(
                    &rate_limiter,
                    &track_label,
                    &track_id,
                    &event_emitter,
                    &reason,
                )
                .await;
                return Ok(());
            }
        };

        let samples = match self
            .buffer_track(
                channel,
                &pb,
                &metadata,
                &event_emitter,
                &track_label,
                &track_id,
            )
            .await
        {
            Ok(samples) => samples,
            Err(e) => {
                let reason = e.to_string();
                self.fail_with_error(&pb, &track_label, reason.clone());
                event_emitter.emit_failure(&track_id, &track_label, &reason);
                self.backoff_after_failure(
                    &rate_limiter,
                    &track_label,
                    &track_id,
                    &event_emitter,
                    &reason,
                )
                .await;
                return Ok(());
            }
        };

        event_emitter.emit_stage("downloading", "complete", 100.0, &track_id, &track_label);

        tracing::info!("Encoding track: {}", track_label);
        pb.set_message(format!("Encoding {}", track_label));
        event_emitter.emit_stage("encoding", "start", 0.0, &track_id, &track_label);

        let encoder = crate::encoder::get_encoder(options.format);
        let stream = encoder.encode(samples).await?;

        event_emitter.emit_stage("encoding", "complete", 100.0, &track_id, &track_label);

        pb.set_message(format!("Writing {}", track_label));
        tracing::info!("Writing track: {:?} to file: {}", track_label, &path);
        event_emitter.emit_stage("writing", "start", 0.0, &track_id, &track_label);
        stream.write_to_file(&path).await?;
        event_emitter.emit_stage("writing", "complete", 100.0, &track_id, &track_label);

        event_emitter.emit_stage("tagging", "start", 0.0, &track_id, &track_label);
        let tags = metadata.tags().await?;
        let output_path = path.clone();
        encoder::tags::store_tags(path, &tags, options.format).await?;
        event_emitter.emit_stage("tagging", "complete", 100.0, &track_id, &track_label);

        pb.finish_with_message(format!("Downloaded {}", track_label));
        rate_limiter.on_success().await;
        event_emitter.emit_complete(&track_id, &track_label, &output_path);
        Ok(())
    }

    fn add_progress_bar(&self, track: &TrackMetadata) -> ProgressBar {
        let pb = self
            .progress_bar
            .add(ProgressBar::new(track.approx_size() as u64));
        pb.enable_steady_tick(Duration::from_millis(100));
        pb.set_style(ProgressStyle::with_template("{spinner:.green} {msg} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            // Infallible
            .unwrap()
            .with_key("eta", |state: &ProgressState, w: &mut dyn Write| write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap())
            .progress_chars("#>-"));
        pb.set_message(track.to_string());
        pb
    }

    async fn buffer_track(
        &self,
        mut rx: StreamEventChannel,
        pb: &ProgressBar,
        metadata: &TrackMetadata,
        event_emitter: &EventEmitter,
        track_label: &str,
        track_id: &str,
    ) -> Result<Samples> {
        let mut samples = Vec::<i32>::new();
        let mut last_reported = 0.0_f64;
        while let Some(event) = rx.recv().await {
            match event {
                StreamEvent::Write {
                    bytes,
                    total,
                    mut content,
                } => {
                    tracing::trace!("Written {} bytes out of {}", bytes, total);
                    pb.set_position(bytes as u64);
                    samples.append(&mut content);

                    if total > 0 {
                        let progress = (bytes as f64 / total as f64) * 100.0;
                        if progress.is_finite()
                            && (progress - last_reported >= 1.0 || progress >= 99.5)
                        {
                            event_emitter.emit_stage(
                                "downloading",
                                "progress",
                                progress,
                                track_id,
                                track_label,
                            );
                            last_reported = progress;
                        }
                    }
                }
                StreamEvent::Finished => {
                    tracing::info!("Finished downloading track");
                    event_emitter.emit_stage(
                        "downloading",
                        "progress",
                        100.0,
                        track_id,
                        track_label,
                    );
                    break;
                }
                StreamEvent::Error(stream_error) => {
                    tracing::error!("Error while streaming track: {:?}", stream_error);
                    return Err(anyhow::anyhow!("Streaming error: {:?}", stream_error));
                }
                StreamEvent::Retry {
                    attempt,
                    max_attempts,
                } => {
                    tracing::warn!(
                        "Retrying download, attempt {} of {}: {}",
                        attempt,
                        max_attempts,
                        metadata.to_string()
                    );
                    event_emitter.emit_retry(
                        track_id,
                        track_label,
                        "downloading",
                        attempt,
                        max_attempts,
                    );
                    pb.set_message(format!(
                        "Retrying ({}/{}) {}",
                        attempt,
                        max_attempts,
                        metadata.to_string()
                    ));
                }
            }
        }
        Ok(Samples {
            samples,
            ..Default::default()
        })
    }

    fn fail_with_error<S>(&self, pb: &ProgressBar, name: &str, e: S)
    where
        S: Into<String>,
    {
        tracing::error!("Failed to download {}: {}", name, e.into());
        pb.finish_with_message(
            console::style(format!("Failed! {}", name))
                .red()
                .to_string(),
        );
    }

    async fn backoff_after_failure(
        &self,
        rate_limiter: &RateLimiter,
        track_label: &str,
        track_id: &str,
        event_emitter: &EventEmitter,
        reason: &str,
    ) {
        let delay = rate_limiter.on_failure().await;
        if delay.is_zero() {
            return;
        }

        let delay_ms = delay.as_millis() as u64;
        tracing::warn!(
            delay_ms,
            track = track_label,
            "Rate limit backoff triggered after download failure"
        );
        event_emitter.emit_backoff(track_id, track_label, delay_ms, reason);

        let message = format!(
            "[rate-limit] Backing off for {:.1}s after failure: {}",
            delay.as_secs_f32(),
            track_label
        );
        let _ = self.progress_bar.println(message);

        sleep(delay).await;
    }
}
