use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::CellWidth};
use subflux::{
    action::Action,
    app::{App, Command, HomeField, Page, SourceMode},
    config::{Config, LanguageCode},
    error::{AppError, Result},
    event::TaskEvent,
    media::{Ffmpeg, ToolStatus, check_tools},
    pipeline::{BatchJob, BatchSubtitleInput, PipelineJob, SubtitleInput, run_batch, run_pipeline},
    services::Services,
    subtitle::{
        FileSubtitleWriter, SubtitleOutputMode,
        catalog::{discover_subtitles, probe_subtitles},
    },
    translator::{TranslationItem, TranslationRequest, TranslationResponse, Translator},
};
use tokio::{
    sync::mpsc::{UnboundedReceiver, unbounded_channel},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const SOURCE: &str = "1\n00:00:00,000 --> 00:00:00,800\nhello\n";

struct FixtureTranslator;

#[async_trait]
impl Translator for FixtureTranslator {
    async fn translate(
        &self,
        request: TranslationRequest,
        _: &CancellationToken,
    ) -> Result<TranslationResponse> {
        Ok(TranslationResponse {
            entries: request
                .segments
                .into_iter()
                .map(|entry| TranslationItem {
                    id: entry.id,
                    text: format!("translated: {}", entry.text),
                })
                .collect(),
        })
    }
}

fn services() -> Arc<Services> {
    Arc::new(Services {
        translator: Some(Arc::new(FixtureTranslator)),
        stt: None,
        subtitle_writer: Arc::new(FileSubtitleWriter),
        ffmpeg: Arc::new(Ffmpeg),
    })
}

fn job(video: Option<PathBuf>, input: SubtitleInput, force_translation: bool) -> PipelineJob {
    PipelineJob {
        video,
        input,
        source_language: LanguageCode::auto(),
        target_language: LanguageCode::parse("zh-CN").unwrap(),
        output_mode: SubtitleOutputMode::Translated,
        force_translation,
        config: Config::from_map(&HashMap::new()).unwrap(),
    }
}

fn key(app: &mut App, code: KeyCode) -> Vec<Command> {
    app.update(Action::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

async fn finish<T: Send + 'static>(
    task: JoinHandle<Result<T>>,
    mut events: UnboundedReceiver<TaskEvent>,
    approve: bool,
) -> (Result<T>, Vec<&'static str>) {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut app = App::new(
            Config::from_map(&HashMap::new()).unwrap(),
            ToolStatus::default(),
        );
        let mut prompts = Vec::new();
        while let Some(mut event) = events.recv().await {
            if let TaskEvent::BatchVideoEvent { event: inner, .. } = event {
                event = *inner;
            }
            match &event {
                TaskEvent::TargetSubtitleRequested { .. } => prompts.push("translation"),
                TaskEvent::OverwriteRequested { .. }
                | TaskEvent::BatchOverwriteRequested { .. } => prompts.push("overwrite"),
                _ => {}
            }
            app.update(Action::Task(Box::new(event)));
            if app.overwrite_prompt.is_some() {
                key(&mut app, KeyCode::Char(if approve { 'y' } else { 'n' }));
            }
        }
        (task.await.unwrap(), prompts)
    })
    .await
    .expect("pipeline confirmation must not hang")
}

async fn video_fixture(directory: &Path, name: &str) -> PathBuf {
    let source = directory.join("fixture-source.srt");
    tokio::fs::write(&source, SOURCE).await.unwrap();
    let video = directory.join(format!("{name}.mkv"));
    let result = tokio::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=1",
            "-i",
        ])
        .arg(&source)
        .arg("-i")
        .arg(&source)
        .args([
            "-map",
            "0:v",
            "-map",
            "1:s",
            "-map",
            "2:s",
            "-c:v",
            "ffv1",
            "-c:s",
            "srt",
            "-metadata:s:s:0",
            "language=eng",
            "-metadata:s:s:1",
            "language=zho",
        ])
        .arg(&video)
        .output()
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    video
}

#[tokio::test]
async fn unified_selection_translates_the_chosen_external_subtitle() {
    if !check_tools().is_ready() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let video = video_fixture(directory.path(), "movie").await;
    let external = directory.path().join("movie.en.srt");
    tokio::fs::write(&external, SOURCE.replace("hello", "external choice"))
        .await
        .unwrap();
    tokio::fs::write(directory.path().join("movie-other.en.srt"), SOURCE)
        .await
        .unwrap();
    let mut app = App::new(
        Config::from_map(&HashMap::new()).unwrap(),
        ToolStatus::default(),
    );
    app.video_path = video.display().to_string();
    let commands = key(&mut app, KeyCode::Char('t'));
    let [Command::Probe { request_id, .. }] = commands.as_slice() else {
        panic!("expected subtitle probe")
    };
    let (probe, external_subtitles) = probe_subtitles(&video).await.unwrap();
    app.update(Action::Task(Box::new(TaskEvent::ProbeSucceeded {
        request_id: *request_id,
        probe,
        external_subtitles,
    })));
    assert_eq!(app.subtitle_count(), 3);
    assert!(app.subtitle_label(1).unwrap().contains("目标语言"));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.source_mode, SourceMode::Embedded);
    assert_eq!(
        app.selected_track,
        Some(app.tracks.subtitle_tracks[1].index)
    );
    key(&mut app, KeyCode::Char('t'));
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.source_mode, SourceMode::External);
    assert_eq!(app.external_subtitle_path, external.display().to_string());
    key(&mut app, KeyCode::Char('f'));
    assert!(app.force_translation);
    key(&mut app, KeyCode::Char('f'));
    assert!(!app.force_translation);
    app.home_field = HomeField::Start;
    let commands = key(&mut app, KeyCode::Enter);
    let [Command::Start { job, cancellation }] = commands.as_slice() else {
        panic!("expected translation job")
    };
    let (events, received) = unbounded_channel();
    let (result, prompts) = finish(
        tokio::spawn(run_pipeline(
            *job.clone(),
            services(),
            cancellation.clone(),
            events,
        )),
        received,
        true,
    )
    .await;
    assert_eq!(prompts, ["translation"]);
    let output = result.unwrap();
    assert!(
        tokio::fs::read_to_string(output)
            .await
            .unwrap()
            .contains("translated: external choice")
    );
    assert!(
        tokio::fs::read_to_string(external)
            .await
            .unwrap()
            .contains("external choice")
    );
}

#[tokio::test]
async fn target_language_embedded_track_can_be_selected_and_retranslated() {
    if !check_tools().is_ready() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let video = video_fixture(directory.path(), "movie").await;
    let (probe, _) = probe_subtitles(&video).await.unwrap();
    let track = probe
        .subtitle_tracks
        .iter()
        .find(|track| track.language.as_deref() == Some("zho"))
        .unwrap();
    let (events, received) = unbounded_channel();
    let (result, prompts) = finish(
        tokio::spawn(run_pipeline(
            job(Some(video), SubtitleInput::Embedded(track.index), false),
            services(),
            CancellationToken::new(),
            events,
        )),
        received,
        true,
    )
    .await;
    assert_eq!(prompts, ["translation"]);
    assert!(
        tokio::fs::read_to_string(result.unwrap())
            .await
            .unwrap()
            .contains("translated: hello")
    );
}

#[tokio::test]
async fn same_path_source_is_preserved_and_unused_output_name_is_chosen() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("movie.zh-CN.srt");
    tokio::fs::write(&source, SOURCE).await.unwrap();
    tokio::fs::write(
        directory.path().join("movie.zh-CN.translated.srt"),
        "preserve earlier translation",
    )
    .await
    .unwrap();
    let (events, received) = unbounded_channel();
    let (result, prompts) = finish(
        tokio::spawn(run_pipeline(
            job(None, SubtitleInput::External(source.clone()), false),
            services(),
            CancellationToken::new(),
            events,
        )),
        received,
        true,
    )
    .await;
    assert_eq!(prompts, ["translation"]);
    assert_eq!(
        result.unwrap(),
        directory.path().join("movie.zh-CN.translated.2.srt")
    );
    assert_eq!(tokio::fs::read_to_string(&source).await.unwrap(), SOURCE);
    assert_eq!(
        tokio::fs::read_to_string(directory.path().join("movie.zh-CN.translated.srt"))
            .await
            .unwrap(),
        "preserve earlier translation"
    );
}

#[tokio::test]
async fn force_translation_still_requires_output_overwrite_approval() {
    let directory = tempfile::tempdir().unwrap();
    let video = directory.path().join("movie.mkv");
    let source = directory.path().join("movie.en.srt");
    let output = directory.path().join("movie.zh-CN.srt");
    tokio::fs::write(&source, SOURCE).await.unwrap();
    tokio::fs::write(&output, "preserve output").await.unwrap();
    tokio::fs::write(directory.path().join("movie.zh-CN.vtt"), "WEBVTT\n")
        .await
        .unwrap();
    let (events, received) = unbounded_channel();
    let (result, prompts) = finish(
        tokio::spawn(run_pipeline(
            job(Some(video), SubtitleInput::External(source), true),
            services(),
            CancellationToken::new(),
            events,
        )),
        received,
        false,
    )
    .await;
    assert_eq!(prompts, ["overwrite"]);
    assert!(matches!(result, Err(AppError::OutputExists(_))));
    assert_eq!(
        tokio::fs::read_to_string(output).await.unwrap(),
        "preserve output"
    );
}

#[tokio::test]
async fn language_suffixed_sidecar_prompts_but_does_not_automatically_skip() {
    let directory = tempfile::tempdir().unwrap();
    let video = directory.path().join("movie.mkv");
    tokio::fs::write(&video, []).await.unwrap();
    let source = directory.path().join("movie.en.srt");
    tokio::fs::write(&source, SOURCE).await.unwrap();
    tokio::fs::write(directory.path().join("movie.zh-CN.vtt"), "WEBVTT\n")
        .await
        .unwrap();
    for force in [false, true] {
        let (events, received) = unbounded_channel();
        let (result, prompts) = finish(
            tokio::spawn(run_pipeline(
                job(
                    Some(video.clone()),
                    SubtitleInput::External(source.clone()),
                    force,
                ),
                services(),
                CancellationToken::new(),
                events,
            )),
            received,
            true,
        )
        .await;
        assert_eq!(prompts, if force { vec![] } else { vec!["translation"] });
        tokio::fs::remove_file(result.unwrap()).await.unwrap();
    }
}

#[tokio::test]
async fn standalone_external_source_confirms_cross_format_target_subtitles() {
    for approve in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("movie.en.srt");
        let target = directory.path().join("movie.zh-CN.vtt");
        let output = directory.path().join("movie.zh-CN.srt");
        tokio::fs::write(&source, SOURCE).await.unwrap();
        tokio::fs::write(&target, "WEBVTT\\n").await.unwrap();
        let (events, received) = unbounded_channel();
        let (result, prompts) = finish(
            tokio::spawn(run_pipeline(
                job(None, SubtitleInput::External(source), false),
                services(),
                CancellationToken::new(),
                events,
            )),
            received,
            approve,
        )
        .await;
        assert_eq!(prompts, ["translation"]);
        if approve {
            assert!(result.is_ok());
            assert!(
                tokio::fs::read_to_string(output)
                    .await
                    .unwrap()
                    .contains("translated: hello")
            );
        } else {
            assert!(matches!(result, Err(AppError::Cancelled)));
            assert!(!output.exists());
        }
        assert_eq!(
            tokio::fs::read_to_string(target).await.unwrap(),
            "WEBVTT\\n"
        );
    }
}

#[tokio::test]
async fn standalone_sidecar_association_respects_dotted_source_stem_and_force() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("movie.v1.en.srt");
    let target = directory.path().join("movie.v1.zh-CN.vtt");
    let output = directory.path().join("movie.v1.zh-CN.srt");
    tokio::fs::write(&source, SOURCE).await.unwrap();
    tokio::fs::write(&target, "WEBVTT\\n").await.unwrap();
    tokio::fs::write(directory.path().join("movie.v10.zh-CN.vtt"), "WEBVTT\\n")
        .await
        .unwrap();
    tokio::fs::write(
        directory.path().join("movie.v1-extra.zh-CN.vtt"),
        "WEBVTT\\n",
    )
    .await
    .unwrap();

    let (events, received) = unbounded_channel();
    let (result, prompts) = finish(
        tokio::spawn(run_pipeline(
            job(None, SubtitleInput::External(source.clone()), false),
            services(),
            CancellationToken::new(),
            events,
        )),
        received,
        true,
    )
    .await;
    assert_eq!(prompts, ["translation"]);
    assert!(result.is_ok());
    tokio::fs::remove_file(&output).await.unwrap();

    let (events, received) = unbounded_channel();
    let (result, prompts) = finish(
        tokio::spawn(run_pipeline(
            job(None, SubtitleInput::External(source.clone()), true),
            services(),
            CancellationToken::new(),
            events,
        )),
        received,
        true,
    )
    .await;
    assert!(prompts.is_empty());
    assert!(result.is_ok());
    tokio::fs::remove_file(&output).await.unwrap();
    tokio::fs::remove_file(target).await.unwrap();

    let (events, received) = unbounded_channel();
    let (result, prompts) = finish(
        tokio::spawn(run_pipeline(
            job(None, SubtitleInput::External(source), false),
            services(),
            CancellationToken::new(),
            events,
        )),
        received,
        true,
    )
    .await;
    assert!(prompts.is_empty());
    assert!(result.is_ok());
}

#[tokio::test]
async fn batch_shares_translation_confirmation_and_accepts_force_option() {
    if !check_tools().is_ready() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let videos = vec![
        video_fixture(directory.path(), "one").await,
        video_fixture(directory.path(), "two").await,
    ];
    let mut config = Config::from_map(&HashMap::new()).unwrap();
    config.batch_concurrency = 2;
    for (force, approve) in [(false, false), (false, true), (true, true)] {
        let batch = BatchJob {
            videos: videos.clone(),
            subtitle_input: BatchSubtitleInput::Auto,
            source_language: LanguageCode::auto(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Translated,
            force_translation: force,
            config: config.clone(),
        };
        let (events, received) = unbounded_channel();
        let (result, prompts) = finish(
            tokio::spawn(run_batch(
                batch,
                services(),
                CancellationToken::new(),
                events,
            )),
            received,
            approve,
        )
        .await;
        assert_eq!(prompts, if force { vec![] } else { vec!["translation"] });
        let summary = result.unwrap();
        assert_eq!(summary.succeeded.len(), if approve { 2 } else { 0 });
        assert_eq!(summary.skipped.len(), if approve { 0 } else { 2 });
        assert_eq!(summary.failed, [] as [subflux::event::BatchFailure; 0]);
        for output in summary.succeeded {
            tokio::fs::remove_file(output).await.unwrap();
        }
    }
}

#[tokio::test]
async fn simplified_and_traditional_sidecars_remain_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let video = directory.path().join("movie.mkv");
    tokio::fs::write(&video, []).await.unwrap();
    let source = directory.path().join("movie.en.srt");
    tokio::fs::write(&source, SOURCE).await.unwrap();
    tokio::fs::write(directory.path().join("movie.zh-TW.srt"), SOURCE)
        .await
        .unwrap();
    let files = discover_subtitles(&video).await.unwrap();
    assert_eq!(files.len(), 2);
    let (events, received) = unbounded_channel();
    let (result, prompts) = finish(
        tokio::spawn(run_pipeline(
            job(Some(video), SubtitleInput::External(source), false),
            services(),
            CancellationToken::new(),
            events,
        )),
        received,
        true,
    )
    .await;
    assert_eq!(prompts, [] as [&str; 0]);
    assert!(result.is_ok());
}

#[tokio::test]
async fn tiny_terminal_keeps_retranslation_choices_visible() {
    let mut app = App::new(
        Config::from_map(&HashMap::new()).unwrap(),
        ToolStatus::default(),
    );
    app.page = Page::Processing;
    let (response, mut received) = unbounded_channel();
    app.update(Action::Task(Box::new(TaskEvent::TargetSubtitleRequested {
        subject: PathBuf::from("movie.mkv"),
        subtitle: "existing target subtitle".into(),
        batch: false,
        response,
    })));
    let mut terminal = Terminal::new(TestBackend::new(30, 5)).unwrap();
    terminal
        .draw(|frame| subflux::tui::ui::render(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..5 {
        let mut skip = 0_u16;
        for x in 0..30 {
            let cell = buffer.cell((x, y)).unwrap();
            if skip == 0 {
                text.push_str(cell.symbol());
            }
            skip = skip.max(cell.cell_width()).saturating_sub(1);
        }
        text.push('\n');
    }
    assert!(text.contains("Y：翻译"));
    assert!(text.contains("N：跳过"));
    key(&mut app, KeyCode::Char('y'));
    assert_eq!(received.recv().await, Some(true));
}

#[cfg(unix)]
#[tokio::test]
async fn source_aliases_are_preserved_when_force_translation_is_enabled() {
    for symlink in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("movie.mkv");
        let source = directory.path().join("selected.en.srt");
        let output = directory.path().join("movie.zh-CN.srt");
        tokio::fs::write(&video, []).await.unwrap();
        tokio::fs::write(&source, SOURCE).await.unwrap();
        if symlink {
            std::os::unix::fs::symlink(&source, &output).unwrap();
        } else {
            tokio::fs::hard_link(&source, &output).await.unwrap();
        }
        assert_eq!(discover_subtitles(&video).await.unwrap().len(), 1);
        let (events, received) = unbounded_channel();
        let (result, prompts) = finish(
            tokio::spawn(run_pipeline(
                job(Some(video), SubtitleInput::External(source.clone()), true),
                services(),
                CancellationToken::new(),
                events,
            )),
            received,
            true,
        )
        .await;
        assert_eq!(prompts, [] as [&str; 0]);
        assert_eq!(
            result.unwrap(),
            directory.path().join("movie.zh-CN.translated.srt")
        );
        assert_eq!(tokio::fs::read_to_string(source).await.unwrap(), SOURCE);
        assert_eq!(tokio::fs::read_to_string(output).await.unwrap(), SOURCE);
    }
}
