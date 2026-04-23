use std::{env, path::PathBuf, process};

use directories::ProjectDirs;
use serde_json::json;
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
        } => {
            let database_path = database_path.unwrap_or_else(default_database_path);
            let service = match SpotifydlService::open(&database_path) {
                Ok(service) => service,
                Err(error) => {
                    eprintln!(
                        "failed to open service at {}: {error}",
                        database_path.display()
                    );
                    process::exit(1);
                }
            };

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
                    serde_json::to_string_pretty(&payload)
                        .expect("status payload should serialize")
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
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CliCommand {
    Status {
        database_path: Option<PathBuf>,
        json_output: bool,
        require_ready: bool,
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
                        other => {
                            return Err(format!("unrecognized argument: {other}"));
                        }
                    }
                }

                Ok(Self::Status {
                    database_path,
                    json_output,
                    require_ready,
                })
            }
            other => Err(format!("unrecognized command: {other}")),
        }
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
    fn renders_backend_status() {
        assert_eq!(
            render_backend_status(true, "Ready (spotify-dl)"),
            "ready: Ready (spotify-dl)"
        );
        assert_eq!(render_backend_status(false, ""), "not ready");
    }
}
