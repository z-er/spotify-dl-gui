use std::{
    collections::HashSet,
    env,
    path::PathBuf,
    process, thread,
    time::{Duration, Instant},
};

use directories::ProjectDirs;
use serde_json::json;
use spotifydl_protocol::{BackendKind, JobOptions, JobState, ServiceCommand};
use spotifydl_service::SpotifydlService;

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let command = match CliCommand::parse(&args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            print_usage();
            process::exit(2);
        }
    };

    match command {
        CliCommand::Status {
            database_path,
            json_output,
            require_ready,
        } => run_status(database_path, json_output, require_ready),
        CliCommand::ConfigureBackend {
            database_path,
            backend,
            executable,
        } => run_configure_backend(database_path, backend, executable),
        CliCommand::RunUrls {
            database_path,
            urls,
            destination,
            timeout_seconds,
        } => run_urls(database_path, urls, destination, timeout_seconds),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CliCommand {
    Status {
        database_path: Option<PathBuf>,
        json_output: bool,
        require_ready: bool,
    },
    ConfigureBackend {
        database_path: Option<PathBuf>,
        backend: BackendKind,
        executable: Option<String>,
    },
    RunUrls {
        database_path: Option<PathBuf>,
        urls: Vec<String>,
        destination: Option<String>,
        timeout_seconds: u64,
    },
}

impl CliCommand {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut index = 0usize;
        let mut command = "status";

        if let Some(first) = args.first() {
            if !first.starts_with('-') {
                command = first.as_str();
                index = 1;
            }
        }

        match command {
            "status" => {
                let mut database_path = None;
                let mut json_output = false;
                let mut require_ready = false;

                while index < args.len() {
                    match args[index].as_str() {
                        "--json" => {
                            json_output = true;
                            index += 1;
                        }
                        "--require-ready" => {
                            require_ready = true;
                            index += 1;
                        }
                        "--database" => {
                            let Some(path) = args.get(index + 1) else {
                                return Err("--database requires a path".to_string());
                            };
                            database_path = Some(PathBuf::from(path));
                            index += 2;
                        }
                        "--help" | "-h" => {
                            print_usage();
                            process::exit(0);
                        }
                        other => return Err(format!("unrecognized argument: {other}")),
                    }
                }

                Ok(Self::Status {
                    database_path,
                    json_output,
                    require_ready,
                })
            }
            "configure-backend" => {
                let mut database_path = None;
                let mut backend = None;
                let mut executable = None;

                while index < args.len() {
                    match args[index].as_str() {
                        "--database" => {
                            let Some(path) = args.get(index + 1) else {
                                return Err("--database requires a path".to_string());
                            };
                            database_path = Some(PathBuf::from(path));
                            index += 2;
                        }
                        "--backend" => {
                            let Some(value) = args.get(index + 1) else {
                                return Err("--backend requires a value".to_string());
                            };
                            backend = Some(parse_backend_kind(value)?);
                            index += 2;
                        }
                        "--executable" => {
                            let Some(value) = args.get(index + 1) else {
                                return Err("--executable requires a value".to_string());
                            };
                            executable = Some(value.clone());
                            index += 2;
                        }
                        "--help" | "-h" => {
                            print_usage();
                            process::exit(0);
                        }
                        other => return Err(format!("unrecognized argument: {other}")),
                    }
                }

                let Some(backend) = backend else {
                    return Err("--backend is required".to_string());
                };

                Ok(Self::ConfigureBackend {
                    database_path,
                    backend,
                    executable,
                })
            }
            "run-urls" => {
                let mut database_path = None;
                let mut destination = None;
                let mut timeout_seconds = 180u64;
                let mut urls = Vec::new();

                while index < args.len() {
                    match args[index].as_str() {
                        "--database" => {
                            let Some(path) = args.get(index + 1) else {
                                return Err("--database requires a path".to_string());
                            };
                            database_path = Some(PathBuf::from(path));
                            index += 2;
                        }
                        "--destination" => {
                            let Some(path) = args.get(index + 1) else {
                                return Err("--destination requires a path".to_string());
                            };
                            destination = Some(path.clone());
                            index += 2;
                        }
                        "--timeout-seconds" => {
                            let Some(value) = args.get(index + 1) else {
                                return Err("--timeout-seconds requires a value".to_string());
                            };
                            timeout_seconds = value
                                .parse::<u64>()
                                .map_err(|_| "--timeout-seconds must be an integer".to_string())?;
                            index += 2;
                        }
                        "--help" | "-h" => {
                            print_usage();
                            process::exit(0);
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unrecognized argument: {value}"));
                        }
                        value => {
                            urls.push(value.to_string());
                            index += 1;
                        }
                    }
                }

                if urls.is_empty() {
                    return Err("run-urls requires at least one Spotify URL".to_string());
                }

                Ok(Self::RunUrls {
                    database_path,
                    urls,
                    destination,
                    timeout_seconds,
                })
            }
            other => Err(format!("unrecognized command: {other}")),
        }
    }
}

fn run_status(database_path: Option<PathBuf>, json_output: bool, require_ready: bool) {
    let database_path = database_path.unwrap_or_else(default_database_path);
    let service = open_service_or_exit(&database_path);
    let snapshot = service.snapshot();
    let active_job = snapshot
        .queue
        .active_job_id
        .as_ref()
        .map(|job_id| job_id.to_string());

    if json_output {
        let payload = json!({
            "database_path": service.database_path().display().to_string(),
            "backend_name": snapshot.service_health.backend_name,
            "backend_ready": snapshot.service_health.backend_ready,
            "backend_status_message": snapshot.service_health.backend_status_message,
            "preferred_backend": format!("{:?}", snapshot.settings.preferred_backend),
            "external_backend_executable": snapshot.settings.external_backend_executable,
            "queue_status": format!("{:?}", snapshot.queue.status),
            "active_job_id": active_job,
            "queue_jobs": snapshot.queue.jobs.len(),
            "history_jobs": snapshot.history.len(),
            "log_entries": snapshot.logs.len(),
            "default_destination": snapshot.settings.default_destination,
            "default_format": snapshot.settings.default_format,
            "max_parallel": snapshot.settings.max_parallel,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).expect("status payload should serialize")
        );
    } else {
        println!("spotifydl CLI");
        println!("database: {}", service.database_path().display());
        println!("backend: {}", snapshot.service_health.backend_name);
        println!(
            "backend status: {}",
            render_backend_status(
                snapshot.service_health.backend_ready,
                &snapshot.service_health.backend_status_message
            )
        );
        println!(
            "preferred backend: {:?}",
            snapshot.settings.preferred_backend
        );
        println!(
            "external executable: {}",
            blank_to_placeholder(
                &snapshot.settings.external_backend_executable,
                "(auto-discover bundled/PATH)"
            )
        );
        println!("queue status: {:?}", snapshot.queue.status);
        println!("active job: {}", active_job.as_deref().unwrap_or("(none)"));
        println!("queue jobs: {}", snapshot.queue.jobs.len());
        println!("history jobs: {}", snapshot.history.len());
        println!("log entries: {}", snapshot.logs.len());
        println!(
            "defaults: destination={} format={} max_parallel={}",
            blank_to_placeholder(&snapshot.settings.default_destination, "(empty)"),
            blank_to_placeholder(&snapshot.settings.default_format, "(empty)"),
            snapshot.settings.max_parallel,
        );
    }

    if require_ready && !snapshot.service_health.backend_ready {
        eprintln!(
            "backend is not ready: {}",
            render_backend_status(
                snapshot.service_health.backend_ready,
                &snapshot.service_health.backend_status_message
            )
        );
        process::exit(3);
    }
}

fn run_configure_backend(
    database_path: Option<PathBuf>,
    backend: BackendKind,
    executable: Option<String>,
) {
    let database_path = database_path.unwrap_or_else(default_database_path);
    let mut service = open_service_or_exit(&database_path);
    let mut settings = service.snapshot().settings.clone();
    settings.preferred_backend = backend;
    settings.external_backend_executable = executable.unwrap_or_default();

    if let Err(error) = service.dispatch(ServiceCommand::UpdateSettings { settings }) {
        eprintln!(
            "failed to update backend settings at {}: {error}",
            database_path.display()
        );
        process::exit(1);
    }

    let snapshot = service.snapshot();
    println!(
        "configured backend: {:?} ({})",
        snapshot.settings.preferred_backend,
        render_backend_status(
            snapshot.service_health.backend_ready,
            &snapshot.service_health.backend_status_message
        )
    );
    println!(
        "external executable: {}",
        blank_to_placeholder(
            &snapshot.settings.external_backend_executable,
            "(auto-discover bundled/PATH)"
        )
    );
}

fn run_urls(
    database_path: Option<PathBuf>,
    urls: Vec<String>,
    destination: Option<String>,
    timeout_seconds: u64,
) {
    let database_path = database_path.unwrap_or_else(default_database_path);
    let mut service = open_service_or_exit(&database_path);
    let existing_job_ids = service
        .snapshot()
        .queue
        .jobs
        .iter()
        .map(|job| job.id.clone())
        .collect::<HashSet<_>>();

    let options = destination.map(|destination| JobOptions {
        destination,
        ..JobOptions::default()
    });

    if let Err(error) = service.dispatch(ServiceCommand::EnqueueUrls {
        urls,
        source: None,
        options,
    }) {
        eprintln!(
            "failed to enqueue URLs at {}: {error}",
            database_path.display()
        );
        process::exit(1);
    }

    let queued_job_ids = service
        .snapshot()
        .queue
        .jobs
        .iter()
        .filter(|job| !existing_job_ids.contains(&job.id))
        .map(|job| job.id.clone())
        .collect::<Vec<_>>();

    if let Err(error) = service.dispatch(ServiceCommand::StartQueue) {
        eprintln!(
            "failed to start queue at {}: {error}",
            database_path.display()
        );
        process::exit(1);
    }

    let deadline = Instant::now() + Duration::from_secs(timeout_seconds.max(1));
    while Instant::now() < deadline {
        let all_in_history = queued_job_ids.iter().all(|job_id| {
            service
                .snapshot()
                .history
                .iter()
                .any(|entry| &entry.job.id == job_id)
        });
        if all_in_history {
            break;
        }

        if let Err(error) = service.dispatch(ServiceCommand::Tick) {
            eprintln!(
                "failed while ticking queue at {}: {error}",
                database_path.display()
            );
            process::exit(1);
        }

        thread::sleep(Duration::from_millis(250));
    }

    let snapshot = service.snapshot();
    let mut history_jobs = snapshot
        .history
        .iter()
        .filter(|entry| queued_job_ids.iter().any(|job_id| job_id == &entry.job.id))
        .collect::<Vec<_>>();

    if history_jobs.len() != queued_job_ids.len() {
        eprintln!(
            "timed out waiting for queued jobs to finish; completed {}/{} job(s)",
            history_jobs.len(),
            queued_job_ids.len()
        );
        process::exit(4);
    }

    history_jobs.sort_by_key(|entry| entry.job.created_at_ms);
    for entry in &history_jobs {
        println!("job: {}", entry.job.label);
        println!("state: {:?}", entry.job.state);
        println!(
            "totals: completed={} failed={} outputs_written={} flagged_items={}",
            entry.job.totals.items_completed,
            entry.job.totals.items_failed,
            entry.job.totals.outputs_written,
            entry.job.totals.flagged_items
        );
        for item in &entry.job.items {
            println!(
                "item: {} | {:?} | outputs={} | flags={}",
                item.label,
                item.state,
                item.outputs.len(),
                item.flags.len()
            );
        }
        println!("---");
    }

    if history_jobs
        .iter()
        .any(|entry| entry.job.state != JobState::Completed)
    {
        eprintln!("one or more jobs did not complete successfully");
        process::exit(5);
    }
}

fn open_service_or_exit(database_path: &PathBuf) -> SpotifydlService {
    match SpotifydlService::open(database_path) {
        Ok(service) => service,
        Err(error) => {
            eprintln!(
                "failed to open service at {}: {error}",
                database_path.display()
            );
            process::exit(1);
        }
    }
}

fn parse_backend_kind(value: &str) -> Result<BackendKind, String> {
    match value.to_ascii_lowercase().as_str() {
        "fake" => Ok(BackendKind::Fake),
        "external" => Ok(BackendKind::External),
        other => Err(format!(
            "unsupported backend `{other}`; expected `fake` or `external`"
        )),
    }
}

fn default_database_path() -> PathBuf {
    if let Some(project_dirs) = ProjectDirs::from("dev", "spotifydl", "spotifydl-rust") {
        return project_dirs.data_dir().join("spotifydl-rust.sqlite");
    }

    PathBuf::from(".spotifydl-rust/spotifydl-rust.sqlite")
}

fn render_backend_status(ready: bool, message: &str) -> String {
    let prefix = if ready { "ready" } else { "not ready" };
    let trimmed = message.trim();
    if trimmed.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {trimmed}")
    }
}

fn blank_to_placeholder<'a>(value: &'a str, placeholder: &'a str) -> &'a str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        placeholder
    } else {
        trimmed
    }
}

fn print_usage() {
    eprintln!("Usage:");
    eprintln!(
        "  cargo run -p spotifydl-cli -- status [--json] [--require-ready] [--database PATH]"
    );
    eprintln!(
        "  cargo run -p spotifydl-cli -- configure-backend --backend <fake|external> [--executable PATH] [--database PATH]"
    );
    eprintln!(
        "  cargo run -p spotifydl-cli -- run-urls [--database PATH] [--destination PATH] [--timeout-seconds N] <spotify-url>..."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_flags() {
        let args = vec![
            "status".to_string(),
            "--json".to_string(),
            "--require-ready".to_string(),
            "--database".to_string(),
            "C:/data/app.sqlite".to_string(),
        ];

        let command = CliCommand::parse(&args).expect("status args should parse");
        assert_eq!(
            command,
            CliCommand::Status {
                database_path: Some(PathBuf::from("C:/data/app.sqlite")),
                json_output: true,
                require_ready: true,
            }
        );
    }

    #[test]
    fn parses_configure_backend_flags() {
        let args = vec![
            "configure-backend".to_string(),
            "--backend".to_string(),
            "external".to_string(),
            "--executable".to_string(),
            "spotify-dl.exe".to_string(),
            "--database".to_string(),
            "C:/data/app.sqlite".to_string(),
        ];

        let command = CliCommand::parse(&args).expect("configure args should parse");
        assert_eq!(
            command,
            CliCommand::ConfigureBackend {
                database_path: Some(PathBuf::from("C:/data/app.sqlite")),
                backend: BackendKind::External,
                executable: Some("spotify-dl.exe".to_string()),
            }
        );
    }

    #[test]
    fn defaults_to_status_command() {
        let command = CliCommand::parse(&[]).expect("empty args should parse");
        assert_eq!(
            command,
            CliCommand::Status {
                database_path: None,
                json_output: false,
                require_ready: false,
            }
        );
    }

    #[test]
    fn parses_backend_kind() {
        assert_eq!(parse_backend_kind("fake"), Ok(BackendKind::Fake));
        assert_eq!(parse_backend_kind("external"), Ok(BackendKind::External));
        assert!(parse_backend_kind("invalid").is_err());
    }

    #[test]
    fn parses_run_urls_flags() {
        let args = vec![
            "run-urls".to_string(),
            "--database".to_string(),
            "C:/data/app.sqlite".to_string(),
            "--destination".to_string(),
            "C:/music".to_string(),
            "--timeout-seconds".to_string(),
            "90".to_string(),
            "https://open.spotify.com/track/abc".to_string(),
            "https://open.spotify.com/track/def".to_string(),
        ];

        let command = CliCommand::parse(&args).expect("run-urls args should parse");
        assert_eq!(
            command,
            CliCommand::RunUrls {
                database_path: Some(PathBuf::from("C:/data/app.sqlite")),
                urls: vec![
                    "https://open.spotify.com/track/abc".to_string(),
                    "https://open.spotify.com/track/def".to_string(),
                ],
                destination: Some("C:/music".to_string()),
                timeout_seconds: 90,
            }
        );
    }

    #[test]
    fn renders_backend_status() {
        assert_eq!(
            render_backend_status(true, "Ready (spotify-dl)"),
            "ready: Ready (spotify-dl)"
        );
        assert_eq!(render_backend_status(false, ""), "not ready");
    }
}
