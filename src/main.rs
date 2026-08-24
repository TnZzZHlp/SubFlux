use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use tokio::sync::mpsc::unbounded_channel;
use tracing::{error, warn};
use tracing_subscriber::EnvFilter;

use subflux::{
    action::Action,
    app::{App, Command},
    config::Config,
    error::AppError,
    event::TaskEvent,
    media::{check_tools, discover_videos, probe_media},
    pipeline::{run_batch, run_pipeline},
    services::Services,
    tui::{TuiTerminal, spawn_input},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();
    let startup_path = parse_startup_path(env::args_os().skip(1))?;
    let (config, config_problem) = match Config::load() {
        Ok(config) => (config, None),
        Err(error) => (
            Config::from_map(&HashMap::new()).expect("built-in default configuration is valid"),
            Some(format!("无法加载 .env：{}", error.safe_message())),
        ),
    };
    let mut app = App::new(config, check_tools());
    if let Some(problem) = config_problem {
        append_status_message(&mut app, &problem);
    }
    if let Some(path) = startup_path {
        match discover_videos(&path) {
            Ok(videos) if videos.is_empty() => append_status_message(
                &mut app,
                &format!("启动路径中未找到支持的视频文件：{}", path.display()),
            ),
            Ok(videos) => app.set_video_candidates(videos),
            Err(error) => append_status_message(
                &mut app,
                &format!(
                    "无法扫描启动路径 {}：{}",
                    path.display(),
                    error.safe_message()
                ),
            ),
        }
    }

    let mut terminal = TuiTerminal::enter()?;
    let (action_sender, mut actions) = unbounded_channel();
    let (task_sender, mut tasks) = unbounded_channel();
    spawn_input(action_sender);

    let mut running = true;
    while running {
        terminal.draw(&app)?;
        let commands = tokio::select! {
            action = actions.recv() => action.map_or_else(
                || vec![Command::Quit],
                |action| app.update(action),
            ),
            event = tasks.recv() => event.map_or_else(
                Vec::new,
                |event| app.update(Action::Task(Box::new(event))),
            ),
            () = tokio::time::sleep(Duration::from_millis(250)) => app.update(Action::Tick),
        };
        for command in commands {
            if !execute(command, task_sender.clone()) {
                running = false;
                break;
            }
        }
    }
    Ok(())
}

fn parse_startup_path(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<Option<PathBuf>, AppError> {
    let path = arguments.next().map(PathBuf::from);
    if arguments.next().is_some() {
        return Err(AppError::InvalidConfig(
            "用法：subtitle-translator [路径]".into(),
        ));
    }
    Ok(path)
}

fn append_status_message(app: &mut App, message: &str) {
    app.status_message = Some(app.status_message.take().map_or_else(
        || message.into(),
        |existing| format!("{existing}; {message}"),
    ));
}

fn batch_retry_event(
    failed_index: usize,
    result: std::result::Result<PathBuf, AppError>,
) -> TaskEvent {
    match result {
        Ok(output) => TaskEvent::BatchRetrySucceeded {
            failed_index,
            output,
        },
        Err(AppError::OutputExists(_) | AppError::Skipped(_)) => {
            TaskEvent::BatchRetrySkipped { failed_index }
        }
        Err(AppError::Cancelled) => TaskEvent::BatchRetryCancelled,
        Err(error) => TaskEvent::BatchRetryFailed {
            failed_index,
            error: error.safe_message(),
        },
    }
}

fn execute(command: Command, events: tokio::sync::mpsc::UnboundedSender<TaskEvent>) -> bool {
    match command {
        Command::Probe { path, request_id } => {
            tokio::spawn(async move {
                match probe_media(&path).await {
                    Ok(probe) => {
                        let _ = events.send(TaskEvent::ProbeSucceeded { request_id, probe });
                    }
                    Err(error) => {
                        let message = error.safe_message();
                        warn!(error = %message, "media probe failed");
                        let _ = events.send(TaskEvent::ProbeFailed {
                            request_id,
                            error: message,
                        });
                    }
                }
            });
            true
        }
        Command::Start { job, cancellation } => {
            tokio::spawn(async move {
                let job = *job;
                let services =
                    match Services::from_config(&job.config, job.output_mode.needs_translation()) {
                        Ok(services) => Arc::new(services),
                        Err(error) => {
                            let message = error.safe_message();
                            error!(error = %message, "provider setup failed");
                            let _ = events.send(TaskEvent::Failed(message));
                            return;
                        }
                    };
                match run_pipeline(job, services, cancellation, events.clone()).await {
                    Ok(output) => {
                        let _ = events.send(TaskEvent::Finished(output));
                    }
                    Err(subflux::error::AppError::Cancelled) => {
                        let _ = events.send(TaskEvent::Cancelled);
                    }
                    Err(error) => {
                        let message = error.safe_message();
                        error!(error = %message, "subtitle pipeline failed");
                        let _ = events.send(TaskEvent::Failed(message));
                    }
                }
            });
            true
        }
        Command::StartBatch { job, cancellation } => {
            tokio::spawn(async move {
                let job = *job;
                let services =
                    match Services::from_config(&job.config, job.output_mode.needs_translation()) {
                        Ok(services) => Arc::new(services),
                        Err(error) => {
                            let message = error.safe_message();
                            error!(error = %message, "batch provider setup failed");
                            let _ = events.send(TaskEvent::Failed(message));
                            return;
                        }
                    };
                match run_batch(job, services, cancellation, events.clone()).await {
                    Ok(summary) => {
                        let _ = events.send(TaskEvent::BatchFinished(summary));
                    }
                    Err(subflux::error::AppError::Cancelled) => {
                        let _ = events.send(TaskEvent::Cancelled);
                    }
                    Err(error) => {
                        let message = error.safe_message();
                        error!(error = %message, "subtitle batch pipeline failed");
                        let _ = events.send(TaskEvent::Failed(message));
                    }
                }
            });
            true
        }
        Command::RetryBatchVideo {
            job,
            failed_index,
            cancellation,
        } => {
            tokio::spawn(async move {
                let job = *job;
                let services =
                    match Services::from_config(&job.config, job.output_mode.needs_translation()) {
                        Ok(services) => Arc::new(services),
                        Err(error) => {
                            let message = error.safe_message();
                            error!(error = %message, "batch retry provider setup failed");
                            let _ = events.send(TaskEvent::BatchRetryFailed {
                                failed_index,
                                error: message,
                            });
                            return;
                        }
                    };
                let event = batch_retry_event(
                    failed_index,
                    run_pipeline(job, services, cancellation, events.clone()).await,
                );
                if let TaskEvent::BatchRetryFailed { error: message, .. } = &event {
                    error!(error = %message, "batch retry pipeline failed");
                }
                let _ = events.send(event);
            });
            true
        }
        Command::Cancel(cancellation) => {
            cancellation.cancel();
            true
        }
        Command::ReloadConfig => {
            tokio::spawn(async move {
                match Config::load() {
                    Ok(config) => {
                        let _ = events.send(TaskEvent::ConfigReloaded(Box::new(config)));
                    }
                    Err(error) => {
                        let message = error.safe_message();
                        warn!(error = %message, "configuration reload failed");
                        let _ = events.send(TaskEvent::ConfigReloadFailed(message));
                    }
                }
            });
            true
        }
        Command::Quit => false,
    }
}

fn init_logging() {
    let path = PathBuf::from("subtitle-translator.log");
    let filter =
        EnvFilter::try_from_env("SUBFLUX_RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    // A fresh file handle per event avoids terminal stderr output and keeps the
    // TUI intact. Paths and command names are logged, never authorization data.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_or_else(|_| LogWriter::Sink(io::sink()), LogWriter::File)
        })
        .try_init();
}

enum LogWriter {
    File(std::fs::File),
    Sink(io::Sink),
}

impl Write for LogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.write(buffer),
            Self::Sink(sink) => sink.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(file) => file.flush(),
            Self::Sink(sink) => sink.flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declined_retry_overwrite_is_recorded_as_skipped() {
        assert!(matches!(
            batch_retry_event(
                2,
                Err(AppError::OutputExists(PathBuf::from("episode.zh-CN.srt"))),
            ),
            TaskEvent::BatchRetrySkipped { failed_index: 2 }
        ));
    }

    #[test]
    fn accepts_at_most_one_startup_path() {
        assert_eq!(
            parse_startup_path(Vec::<OsString>::new().into_iter()).unwrap(),
            None
        );
        assert_eq!(
            parse_startup_path(vec![OsString::from("library")].into_iter()).unwrap(),
            Some(PathBuf::from("library"))
        );
        assert!(matches!(
            parse_startup_path(vec![OsString::from("one"), OsString::from("two")].into_iter()),
            Err(AppError::InvalidConfig(_))
        ));
    }
}
