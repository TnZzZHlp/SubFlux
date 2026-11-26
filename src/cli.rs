use std::{ffi::OsString, path::PathBuf, sync::Arc};

use subflux::{
    config::Config,
    error::{AppError, Result},
    event::TaskEvent,
    media::check_tools,
    pipeline::{PipelineJob, SubtitleInput, run_pipeline},
    services::Services,
    subtitle::SubtitleOutputMode,
};
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;

pub const USAGE: &str = "Usage:\n  subtitle-translator [PATH]\n  subtitle-translator colab [--original-only] [--overwrite] VIDEO\n\nInstall the official Google Colab CLI and sign in once with `colab usage`.\nThis command transcribes on a T4 and translates with your .env unless --original-only is set.";

#[derive(Default)]
struct Options {
    video: PathBuf,
    original_only: bool,
    overwrite: bool,
}

pub async fn run(args: impl Iterator<Item = OsString>) -> Result<()> {
    let Some(options) = parse(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    execute(options).await
}

fn parse(args: impl Iterator<Item = OsString>) -> Result<Option<Options>> {
    let mut options = Options::default();
    let mut accept_flags = true;
    for arg in args {
        match (accept_flags, arg.to_str()) {
            (true, Some("--help" | "-h")) => return Ok(None),
            (true, Some("--original-only")) => options.original_only = true,
            (true, Some("--overwrite")) => options.overwrite = true,
            (true, Some("--")) => accept_flags = false,
            (true, Some(flag)) if flag.starts_with('-') => {
                return Err(AppError::InvalidConfig(format!(
                    "unknown option: {flag}\n{USAGE}"
                )));
            }
            _ if options.video.as_os_str().is_empty() => options.video = PathBuf::from(arg),
            _ => return Err(AppError::InvalidConfig(USAGE.into())),
        }
    }
    if options.video.as_os_str().is_empty() {
        return Err(AppError::InvalidConfig(USAGE.into()));
    }
    Ok(Some(options))
}

async fn execute(options: Options) -> Result<()> {
    if !tokio::fs::metadata(&options.video).await?.is_file() {
        return Err(AppError::InvalidConfig(
            "Colab mode requires a single media file".into(),
        ));
    }
    let video = std::path::absolute(options.video)?;
    check_tools().into_result()?;
    let mut config = Config::load()?;
    let services = Services::for_colab(&mut config, !options.original_only)?;
    let job = PipelineJob {
        video: Some(video),
        input: SubtitleInput::Stt,
        source_language: config.source_language.clone(),
        target_language: config.target_language.clone(),
        output_mode: if options.original_only {
            SubtitleOutputMode::Original
        } else {
            SubtitleOutputMode::Translated
        },
        force_translation: true,
        config,
    };

    let cancellation = CancellationToken::new();
    let (sender, mut events) = unbounded_channel();
    let mut pipeline = std::pin::pin!(run_pipeline(
        job,
        Arc::new(services),
        cancellation.clone(),
        sender,
    ));
    let mut interrupt = std::pin::pin!(tokio::signal::ctrl_c());
    let result = loop {
        tokio::select! {
            biased;
            signal = &mut interrupt, if !cancellation.is_cancelled() => {
                if let Err(error) = signal {
                    eprintln!("Could not listen for Ctrl+C: {error}");
                }
                cancellation.cancel();
            }
            result = &mut pipeline => break result,
            Some(event) = events.recv() => handle_event(event, options.overwrite),
        }
    };
    while let Ok(event) = events.try_recv() {
        handle_event(event, options.overwrite);
    }
    println!("{}", result?.display());
    Ok(())
}

fn handle_event(event: TaskEvent, overwrite: bool) {
    match event {
        TaskEvent::OverwriteRequested { response, .. } => {
            let _ = response.send(overwrite);
        }
        TaskEvent::SttStarted { current, total } => {
            eprintln!("Transcribing audio chunk {}/{total}", current + 1);
        }
        TaskEvent::Warning(message) => eprintln!("{message}"),
        TaskEvent::TranslationStarted { total } => eprintln!("Translating {total} cues"),
        TaskEvent::TranslationProgress {
            completed, total, ..
        } => {
            eprintln!("Translated {completed}/{total} cues");
        }
        _ => {}
    }
}
