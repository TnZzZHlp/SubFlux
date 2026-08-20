use std::{collections::BTreeMap, path::PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::{
    action::Action,
    config::{Config, LanguageCode},
    event::{BatchSummary, TaskEvent},
    media::{MediaProbe, ToolStatus, TrackIndex},
    output::build_output_path,
    pipeline::{BatchJob, BatchSubtitleInput, PipelineJob, SubtitleInput},
    subtitle::{SubtitleFormat, SubtitleOutputMode},
};

const SOURCE_LANGUAGES: &[&str] = &["auto", "ja", "en", "ko", "zh-CN", "zh-TW", "fr", "de", "es"];
const TARGET_LANGUAGES: &[&str] = &["zh-CN", "zh-TW", "en", "ja", "ko", "fr", "de", "es"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Home,
    Videos,
    Settings,
    Tracks,
    Processing,
    Result,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceMode {
    Auto,
    Embedded,
    External,
    Stt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeField {
    Video,
    Source,
    ExternalSubtitle,
    Track,
    SourceLanguage,
    TargetLanguage,
    Output,
    Start,
}

impl HomeField {
    const ALL: [Self; 8] = [
        Self::Video,
        Self::Source,
        Self::ExternalSubtitle,
        Self::Track,
        Self::SourceLanguage,
        Self::TargetLanguage,
        Self::Output,
        Self::Start,
    ];

    fn next(self, backwards: bool) -> Self {
        let index = Self::ALL
            .iter()
            .position(|value| *value == self)
            .unwrap_or(0);
        let length = Self::ALL.len();
        Self::ALL[(index + if backwards { length - 1 } else { 1 }) % length]
    }
}

#[derive(Clone, Debug)]
pub struct ProcessingState {
    pub stage: String,
    pub completed: usize,
    pub total: Option<usize>,
    pub request: Option<usize>,
    pub errors: usize,
    pub batch: Option<BatchProcessingState>,
}

impl Default for ProcessingState {
    fn default() -> Self {
        Self {
            stage: "准备中…".into(),
            completed: 0,
            total: None,
            request: None,
            errors: 0,
            batch: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BatchFileProgress {
    pub video: PathBuf,
    pub stage: String,
    pub completed: usize,
    pub total: Option<usize>,
    pub request: Option<usize>,
}

impl BatchFileProgress {
    fn new(video: PathBuf) -> Self {
        Self {
            video,
            stage: "准备中…".into(),
            completed: 0,
            total: None,
            request: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BatchProcessingState {
    pub total: usize,
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub active: BTreeMap<usize, BatchFileProgress>,
}

#[derive(Clone, Debug, Default)]
pub struct ResultState {
    pub output: Option<PathBuf>,
    pub error: Option<String>,
    pub batch: Option<BatchSummary>,
}

#[derive(Clone, Debug)]
pub struct OverwritePrompt {
    pub output: PathBuf,
    pub batch: bool,
    response: UnboundedSender<bool>,
}

#[derive(Clone, Debug)]
pub struct App {
    pub page: Page,
    pub config: Config,
    pub tools: ToolStatus,
    pub video_path: String,
    pub video_candidates: Vec<PathBuf>,
    pub video_cursor: usize,
    pub external_subtitle_path: String,
    pub source_mode: SourceMode,
    pub selected_track: Option<TrackIndex>,
    pub tracks: MediaProbe,
    pub track_cursor: usize,
    pub source_language: LanguageCode,
    pub target_language: LanguageCode,
    pub output_mode: SubtitleOutputMode,
    pub home_field: HomeField,
    pub processing: ProcessingState,
    pub result: ResultState,
    pub status_message: Option<String>,
    pub overwrite_prompt: Option<OverwritePrompt>,
    cancellation: Option<CancellationToken>,
    processing_scroll: u16,
    result_scroll: u16,
}

#[derive(Clone, Debug)]
pub enum Command {
    Probe(PathBuf),
    Start {
        job: Box<PipelineJob>,
        cancellation: CancellationToken,
    },
    StartBatch {
        job: Box<BatchJob>,
        cancellation: CancellationToken,
    },
    Cancel(CancellationToken),
    ReloadConfig,
    Quit,
}

impl App {
    pub fn new(config: Config, tools: ToolStatus) -> Self {
        let status_message = (!tools.is_ready())
            .then(|| format!("所需媒体工具不可用：{}", tools.problems().join("; ")));
        Self {
            page: Page::Home,
            source_language: config.source_language.clone(),
            target_language: config.target_language.clone(),
            output_mode: SubtitleOutputMode::default(),
            config,
            tools,
            video_path: String::new(),
            video_candidates: Vec::new(),
            video_cursor: 0,
            external_subtitle_path: String::new(),
            source_mode: SourceMode::Auto,
            selected_track: None,
            tracks: MediaProbe::default(),
            track_cursor: 0,
            home_field: HomeField::Video,
            processing: ProcessingState::default(),
            result: ResultState::default(),
            status_message,
            overwrite_prompt: None,
            cancellation: None,
            processing_scroll: 0,
            result_scroll: 0,
        }
    }

    pub fn update(&mut self, action: Action) -> Vec<Command> {
        match action {
            Action::Key(key) => self.handle_key(key),
            Action::Task(event) => self.handle_task(*event),
            Action::Tick => Vec::new(),
        }
    }

    /// Stores recursively discovered videos for either individual selection or
    /// an explicit batch run.
    pub fn set_video_candidates(&mut self, videos: Vec<PathBuf>) {
        self.video_candidates = videos;
        self.video_cursor = 0;
        match self.video_candidates.len() {
            0 => self.status_message = Some("未找到支持的视频文件。".into()),
            1 => self.select_video_candidate(),
            count => {
                self.page = Page::Videos;
                self.status_message = Some(format!(
                    "已找到 {count} 个视频：按 B 批量处理全部，或按 Enter 选择单个视频。"
                ));
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Command> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return self.cancel_or_quit();
        }
        if self.overwrite_prompt.is_some() {
            return self.handle_overwrite_key(key);
        }
        match self.page {
            Page::Home => self.handle_home_key(key),
            Page::Videos => self.handle_videos_key(key),
            Page::Settings => self.handle_settings_key(key),
            Page::Tracks => self.handle_tracks_key(key),
            Page::Processing => self.handle_processing_key(key),
            Page::Result => self.handle_result_key(key),
        }
    }

    fn handle_home_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Char(character) if self.home_text_input_is_active() => {
                self.edit_current_text(|value| value.push(character));
                Vec::new()
            }
            KeyCode::Char('q') => vec![Command::Quit],
            KeyCode::Char('s') => {
                self.page = Page::Settings;
                Vec::new()
            }
            KeyCode::Char('t') => {
                self.page = Page::Tracks;
                Vec::new()
            }
            KeyCode::Char('b') => self.start_batch_command(),
            KeyCode::Char('p') => self.probe_command(),
            KeyCode::Tab | KeyCode::Down => {
                self.home_field = self.home_field.next(false);
                Vec::new()
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.home_field = self.home_field.next(true);
                Vec::new()
            }
            KeyCode::Left => self.adjust_current(-1),
            KeyCode::Right => self.adjust_current(1),
            KeyCode::Enter => self.activate_home_field(),
            KeyCode::Backspace => {
                self.edit_current_text(|value| {
                    value.pop();
                });
                Vec::new()
            }
            KeyCode::Char(character) => {
                self.edit_current_text(|value| value.push(character));
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    const fn home_text_input_is_active(&self) -> bool {
        matches!(
            self.home_field,
            HomeField::Video | HomeField::ExternalSubtitle
        )
    }

    fn handle_settings_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('h') => {
                self.page = Page::Home;
                Vec::new()
            }
            KeyCode::Char('r') => vec![Command::ReloadConfig],
            KeyCode::Char('q') => vec![Command::Quit],
            _ => Vec::new(),
        }
    }

    fn handle_videos_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('h') => {
                self.page = Page::Home;
                Vec::new()
            }
            KeyCode::Up => {
                self.video_cursor = self.video_cursor.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Down => {
                let maximum = self.video_candidates.len().saturating_sub(1);
                self.video_cursor = (self.video_cursor + 1).min(maximum);
                Vec::new()
            }
            KeyCode::Enter => {
                self.select_video_candidate();
                Vec::new()
            }
            KeyCode::Char('b') => self.start_batch_command(),
            KeyCode::Char('q') => vec![Command::Quit],
            _ => Vec::new(),
        }
    }

    fn handle_tracks_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('h') => {
                self.page = Page::Home;
                Vec::new()
            }
            KeyCode::Char('a') => {
                self.source_mode = SourceMode::Auto;
                self.page = Page::Home;
                Vec::new()
            }
            KeyCode::Char('x') => {
                self.source_mode = SourceMode::Stt;
                self.page = Page::Home;
                Vec::new()
            }
            KeyCode::Up => {
                self.track_cursor = self.track_cursor.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Down => {
                let max = self.tracks.subtitle_tracks.len().saturating_sub(1);
                self.track_cursor = (self.track_cursor + 1).min(max);
                Vec::new()
            }
            KeyCode::Enter => {
                if let Some(track) = self.tracks.subtitle_tracks.get(self.track_cursor) {
                    if track.is_text() {
                        self.source_mode = SourceMode::Embedded;
                        self.selected_track = Some(track.index);
                        self.page = Page::Home;
                        self.status_message = Some(format!("已选择 {}", track.display_label()));
                    } else {
                        self.status_message = Some(
                            "当前字幕轨为图像字幕，不支持直接翻译。请选择 STT 模式（按 X）。"
                                .into(),
                        );
                    }
                }
                Vec::new()
            }
            KeyCode::Char('p') => self.probe_command(),
            KeyCode::Char('q') => vec![Command::Quit],
            _ => Vec::new(),
        }
    }

    fn handle_processing_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Char('c') | KeyCode::Esc => self.cancel_or_quit(),
            KeyCode::Up if self.processing.batch.is_some() => {
                self.processing_scroll = self.processing_scroll.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Down if self.processing.batch.is_some() => {
                self.processing_scroll = self.processing_scroll.saturating_add(1);
                Vec::new()
            }
            KeyCode::PageUp if self.processing.batch.is_some() => {
                self.processing_scroll = self.processing_scroll.saturating_sub(10);
                Vec::new()
            }
            KeyCode::PageDown if self.processing.batch.is_some() => {
                self.processing_scroll = self.processing_scroll.saturating_add(10);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_overwrite_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.answer_overwrite(true),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => self.answer_overwrite(false),
            _ => Vec::new(),
        }
    }

    fn answer_overwrite(&mut self, overwrite: bool) -> Vec<Command> {
        let Some(prompt) = self.overwrite_prompt.take() else {
            return Vec::new();
        };
        let output = prompt.output;
        let batch = prompt.batch;
        let _ = prompt.response.send(overwrite);
        self.status_message = Some(match (batch, overwrite) {
            (true, true) => "将覆盖本批量任务中所有已有输出。".into(),
            (true, false) => "将跳过本批量任务中所有已有输出。".into(),
            (false, true) => format!("将覆盖输出：{}", output.display()),
            (false, false) => format!("已跳过输出：{}", output.display()),
        });
        Vec::new()
    }

    fn handle_result_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Up if self.result_has_details() => {
                self.result_scroll = self.result_scroll.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Down if self.result_has_details() => {
                self.result_scroll = self.result_scroll.saturating_add(1);
                Vec::new()
            }
            KeyCode::PageUp if self.result_has_details() => {
                self.result_scroll = self.result_scroll.saturating_sub(10);
                Vec::new()
            }
            KeyCode::PageDown if self.result_has_details() => {
                self.result_scroll = self.result_scroll.saturating_add(10);
                Vec::new()
            }
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('h') => {
                self.page = Page::Home;
                self.result = ResultState::default();
                self.result_scroll = 0;
                Vec::new()
            }
            KeyCode::Char('q') => vec![Command::Quit],
            _ => Vec::new(),
        }
    }

    fn activate_home_field(&mut self) -> Vec<Command> {
        match self.home_field {
            HomeField::Video if !self.video_candidates.is_empty() => {
                self.page = Page::Videos;
                Vec::new()
            }
            HomeField::Source => {
                self.adjust_source(1);
                Vec::new()
            }
            HomeField::Track => {
                self.page = Page::Tracks;
                Vec::new()
            }
            HomeField::SourceLanguage => {
                self.cycle_language(true, 1);
                Vec::new()
            }
            HomeField::TargetLanguage => {
                self.cycle_language(false, 1);
                Vec::new()
            }
            HomeField::Output => {
                self.adjust_output_mode(1);
                Vec::new()
            }
            HomeField::Start => self.start_command(),
            HomeField::Video | HomeField::ExternalSubtitle => Vec::new(),
        }
    }

    fn select_video_candidate(&mut self) {
        let Some(path) = self.video_candidates.get(self.video_cursor) else {
            return;
        };
        let Some(path) = path.to_str() else {
            self.status_message = Some("所选视频路径不是有效的 UTF-8 文本。".into());
            return;
        };
        self.video_path = path.into();
        self.page = Page::Home;
    }

    fn adjust_current(&mut self, direction: i8) -> Vec<Command> {
        match self.home_field {
            HomeField::Source => self.adjust_source(direction),
            HomeField::SourceLanguage => self.cycle_language(true, direction),
            HomeField::TargetLanguage => self.cycle_language(false, direction),
            HomeField::Output => self.adjust_output_mode(direction),
            _ => {}
        }
        Vec::new()
    }

    fn adjust_source(&mut self, direction: i8) {
        let modes = [
            SourceMode::Auto,
            SourceMode::Embedded,
            SourceMode::External,
            SourceMode::Stt,
        ];
        let current = modes
            .iter()
            .position(|mode| *mode == self.source_mode)
            .unwrap_or(0);
        let next = if direction < 0 {
            (current + modes.len() - 1) % modes.len()
        } else {
            (current + 1) % modes.len()
        };
        self.source_mode = modes[next];
    }

    fn cycle_language(&mut self, source: bool, direction: i8) {
        let candidates = if source {
            SOURCE_LANGUAGES
        } else {
            TARGET_LANGUAGES
        };
        let current = if source {
            &self.source_language
        } else {
            &self.target_language
        };
        let index = candidates
            .iter()
            .position(|value| *value == current.as_str())
            .unwrap_or(0);
        let next = if direction < 0 {
            (index + candidates.len() - 1) % candidates.len()
        } else {
            (index + 1) % candidates.len()
        };
        // Constants above are valid BCP 47 values.
        let value = LanguageCode::parse(candidates[next]).expect("built-in language is valid");
        if source {
            self.source_language = value;
        } else {
            self.target_language = value;
        }
    }

    fn adjust_output_mode(&mut self, direction: i8) {
        let modes = [
            SubtitleOutputMode::Translated,
            SubtitleOutputMode::Bilingual,
            SubtitleOutputMode::Original,
        ];
        let current = modes
            .iter()
            .position(|mode| *mode == self.output_mode)
            .unwrap_or(0);
        let next = if direction < 0 {
            (current + modes.len() - 1) % modes.len()
        } else {
            (current + 1) % modes.len()
        };
        self.output_mode = modes[next];
    }

    fn edit_current_text(&mut self, edit: impl FnOnce(&mut String)) {
        match self.home_field {
            HomeField::Video => edit(&mut self.video_path),
            HomeField::ExternalSubtitle => edit(&mut self.external_subtitle_path),
            HomeField::Source
            | HomeField::Track
            | HomeField::SourceLanguage
            | HomeField::TargetLanguage
            | HomeField::Output
            | HomeField::Start => {}
        }
    }

    fn probe_command(&mut self) -> Vec<Command> {
        let value = self.video_path.trim();
        if value.is_empty() {
            self.status_message = Some("请先输入视频路径，再探测字幕轨。".into());
            return Vec::new();
        }
        self.status_message = Some("正在探测字幕轨…".into());
        vec![Command::Probe(PathBuf::from(value))]
    }

    fn start_command(&mut self) -> Vec<Command> {
        if !self.tools.is_ready() {
            self.page = Page::Result;
            self.result = ResultState {
                output: None,
                error: Some(format!(
                    "所需媒体工具不可用，无法开始：{}",
                    self.tools.problems().join("; ")
                )),
                batch: None,
            };
            return Vec::new();
        }
        let video =
            (!self.video_path.trim().is_empty()).then(|| PathBuf::from(self.video_path.trim()));
        let input = match self.source_mode {
            SourceMode::Auto => SubtitleInput::Auto,
            SourceMode::Embedded => {
                if let Some(track) = self.selected_track {
                    SubtitleInput::Embedded(track)
                } else {
                    self.status_message = Some("请先选择文本字幕轨（按 T）。".into());
                    return Vec::new();
                }
            }
            SourceMode::External => {
                if self.external_subtitle_path.trim().is_empty() {
                    self.status_message =
                        Some("请先输入外部 .srt/.ass/.ssa/.vtt 字幕路径。".into());
                    return Vec::new();
                }
                SubtitleInput::External(PathBuf::from(self.external_subtitle_path.trim()))
            }
            SourceMode::Stt => SubtitleInput::Stt,
        };
        if video.is_none() && !matches!(&input, SubtitleInput::External(_)) {
            self.status_message = Some("该字幕来源需要视频路径。".into());
            return Vec::new();
        }
        let cancellation = CancellationToken::new();
        let job = PipelineJob {
            video,
            input,
            source_language: self.source_language.clone(),
            target_language: self.target_language.clone(),
            output_mode: self.output_mode,
            config: self.config.clone(),
        };
        self.processing = ProcessingState::default();
        self.processing_scroll = 0;
        self.result = ResultState::default();
        self.result_scroll = 0;
        self.cancellation = Some(cancellation.clone());
        self.page = Page::Processing;
        vec![Command::Start {
            job: Box::new(job),
            cancellation,
        }]
    }

    fn start_batch_command(&mut self) -> Vec<Command> {
        if !self.tools.is_ready() {
            self.page = Page::Result;
            self.result = ResultState {
                output: None,
                error: Some(format!(
                    "所需媒体工具不可用，无法开始：{}",
                    self.tools.problems().join("; ")
                )),
                batch: None,
            };
            return Vec::new();
        }
        if self.video_candidates.is_empty() {
            self.status_message = Some(
                "请先以文件夹作为启动路径，或在视频选择页发现至少一个视频后再批量处理。".into(),
            );
            return Vec::new();
        }
        let subtitle_input = match self.source_mode {
            SourceMode::Auto => BatchSubtitleInput::Auto,
            SourceMode::Stt => BatchSubtitleInput::Stt,
            SourceMode::Embedded => {
                self.status_message = Some(
                    "批量处理不能复用单个视频的指定字幕轨。请选择“自动”或“语音识别（STT）”。"
                        .into(),
                );
                return Vec::new();
            }
            SourceMode::External => {
                self.status_message = Some(
                    "批量处理不支持一个外部字幕路径对应多个视频。请选择“自动”或“语音识别（STT）”。"
                        .into(),
                );
                return Vec::new();
            }
        };
        let cancellation = CancellationToken::new();
        let total = self.video_candidates.len();
        let job = BatchJob {
            videos: self.video_candidates.clone(),
            subtitle_input,
            source_language: self.source_language.clone(),
            target_language: self.target_language.clone(),
            output_mode: self.output_mode,
            config: self.config.clone(),
        };
        self.processing = ProcessingState::default();
        self.processing_scroll = 0;
        self.result = ResultState::default();
        self.result_scroll = 0;
        self.status_message = Some(format!("正在批量处理 {total} 个视频…"));
        self.cancellation = Some(cancellation.clone());
        self.page = Page::Processing;
        vec![Command::StartBatch {
            job: Box::new(job),
            cancellation,
        }]
    }

    fn cancel_or_quit(&mut self) -> Vec<Command> {
        if let Some(cancellation) = self.cancellation.clone() {
            self.clear_overwrite_prompts();
            self.processing.stage = "正在取消…".into();
            vec![Command::Cancel(cancellation)]
        } else {
            vec![Command::Quit]
        }
    }

    fn clear_overwrite_prompts(&mut self) {
        self.overwrite_prompt = None;
    }

    fn batch_state(&mut self, total: usize) -> &mut BatchProcessingState {
        let batch = self
            .processing
            .batch
            .get_or_insert_with(|| BatchProcessingState {
                total,
                ..BatchProcessingState::default()
            });
        batch.total = total;
        batch
    }

    fn batch_file(
        &mut self,
        current: usize,
        total: usize,
        video: &std::path::Path,
    ) -> &mut BatchFileProgress {
        self.batch_state(total)
            .active
            .entry(current)
            .or_insert_with(|| BatchFileProgress::new(video.to_path_buf()))
    }

    fn handle_batch_video_event(
        &mut self,
        current: usize,
        total: usize,
        video: PathBuf,
        event: TaskEvent,
    ) {
        match event {
            TaskEvent::Probing => {
                self.batch_file(current, total, &video).stage = "正在探测媒体…".into();
            }
            TaskEvent::TracksLoaded(_) => {
                self.batch_file(current, total, &video).stage = "字幕轨已加载…".into();
            }
            TaskEvent::ExtractingSubtitle => {
                self.batch_file(current, total, &video).stage = "正在提取字幕…".into();
            }
            TaskEvent::ExtractingAudio => {
                self.batch_file(current, total, &video).stage = "正在提取音频…".into();
            }
            TaskEvent::SttStarted {
                current: completed,
                total: file_total,
            } => {
                let file = self.batch_file(current, total, &video);
                file.stage = "正在进行语音识别…".into();
                file.completed = completed;
                file.total = Some(file_total);
                file.request = None;
            }
            TaskEvent::SttProgress {
                current: completed,
                total: file_total,
            } => {
                let file = self.batch_file(current, total, &video);
                file.stage = "正在进行语音识别…".into();
                file.completed = completed;
                file.total = file_total;
            }
            TaskEvent::TranslationStarted { total: file_total } => {
                let file = self.batch_file(current, total, &video);
                file.stage = "正在翻译…".into();
                file.completed = 0;
                file.total = Some(file_total);
                file.request = None;
            }
            TaskEvent::TranslationProgress {
                completed,
                total: file_total,
                request,
            } => {
                let file = self.batch_file(current, total, &video);
                file.stage = "正在翻译…".into();
                file.completed = completed;
                file.total = Some(file_total);
                file.request = Some(request);
            }
            TaskEvent::OverwriteRequested { output, response } => {
                self.batch_file(current, total, &video).stage = "等待覆盖确认…".into();
                self.overwrite_prompt = Some(OverwritePrompt {
                    output,
                    batch: false,
                    response,
                });
            }
            TaskEvent::BatchOverwriteRequested { output, response } => {
                self.batch_file(current, total, &video).stage = "等待批量覆盖确认…".into();
                self.overwrite_prompt = Some(OverwritePrompt {
                    output,
                    batch: true,
                    response,
                });
            }
            TaskEvent::Writing => {
                self.batch_file(current, total, &video).stage = "正在写入字幕…".into();
            }
            TaskEvent::BatchStarted { .. }
            | TaskEvent::BatchVideoStarted { .. }
            | TaskEvent::BatchVideoEvent { .. }
            | TaskEvent::BatchVideoSucceeded { .. }
            | TaskEvent::BatchVideoSkipped { .. }
            | TaskEvent::BatchVideoFailed { .. }
            | TaskEvent::Finished(_)
            | TaskEvent::BatchFinished(_)
            | TaskEvent::Failed(_)
            | TaskEvent::Cancelled
            | TaskEvent::ConfigReloaded(_)
            | TaskEvent::ConfigReloadFailed(_) => {}
        }
    }

    fn handle_task(&mut self, event: TaskEvent) -> Vec<Command> {
        match event {
            TaskEvent::BatchStarted { total } => {
                self.processing_scroll = 0;
                self.processing.batch = Some(BatchProcessingState {
                    total,
                    ..BatchProcessingState::default()
                });
                self.processing.stage = "正在准备批量任务…".into();
            }
            TaskEvent::BatchVideoStarted {
                current,
                total,
                video,
            } => {
                self.batch_state(total)
                    .active
                    .insert(current, BatchFileProgress::new(video));
            }
            TaskEvent::BatchVideoEvent {
                current,
                total,
                video,
                event,
            } => self.handle_batch_video_event(current, total, video, *event),
            TaskEvent::BatchVideoSucceeded {
                current,
                total,
                video: _,
                output: _,
            } => {
                let batch = self.batch_state(total);
                batch.active.remove(&current);
                batch.succeeded += 1;
            }
            TaskEvent::BatchVideoSkipped {
                current,
                total,
                video: _,
            } => {
                let batch = self.batch_state(total);
                batch.active.remove(&current);
                batch.skipped += 1;
            }
            TaskEvent::BatchVideoFailed {
                current,
                total,
                video: _,
                error,
            } => {
                let batch = self.batch_state(total);
                batch.active.remove(&current);
                batch.failed += 1;
                self.processing.errors += 1;
                self.status_message = Some(format!("第 {current}/{total} 个视频处理失败：{error}"));
            }
            TaskEvent::Probing => self.processing.stage = "正在探测媒体…".into(),
            TaskEvent::TracksLoaded(probe) => {
                if self.selected_track.is_none() {
                    self.selected_track = probe.auto_track().map(|track| track.index);
                }
                let count = probe.subtitle_tracks.len();
                self.tracks = probe;
                self.status_message = Some(format!("已找到 {count} 条字幕轨。"));
                if self.page == Page::Processing {
                    self.processing.stage = "字幕轨已加载…".into();
                }
            }
            TaskEvent::ExtractingSubtitle => self.processing.stage = "正在提取字幕…".into(),
            TaskEvent::ExtractingAudio => self.processing.stage = "正在提取音频…".into(),
            TaskEvent::SttStarted { current, total } => {
                self.processing.stage = "正在进行语音识别…".into();
                self.processing.completed = current;
                self.processing.total = Some(total);
                self.processing.request = None;
            }
            TaskEvent::SttProgress { current, total } => {
                self.processing.stage = "正在进行语音识别…".into();
                self.processing.completed = current;
                self.processing.total = total;
            }
            TaskEvent::TranslationStarted { total } => {
                self.processing.stage = "正在翻译…".into();
                self.processing.completed = 0;
                self.processing.total = Some(total);
                self.processing.request = None;
            }
            TaskEvent::TranslationProgress {
                completed,
                total,
                request,
            } => {
                self.processing.stage = "正在翻译…".into();
                self.processing.completed = completed;
                self.processing.total = Some(total);
                self.processing.request = Some(request);
            }
            TaskEvent::OverwriteRequested { output, response } => {
                self.processing.stage = "等待覆盖确认…".into();
                self.overwrite_prompt = Some(OverwritePrompt {
                    output,
                    batch: false,
                    response,
                });
            }
            TaskEvent::BatchOverwriteRequested { output, response } => {
                self.processing.stage = "等待批量覆盖确认…".into();
                self.overwrite_prompt = Some(OverwritePrompt {
                    output,
                    batch: true,
                    response,
                });
            }
            TaskEvent::Writing => self.processing.stage = "正在写入字幕…".into(),
            TaskEvent::Finished(output) => {
                self.clear_overwrite_prompts();
                self.cancellation = None;
                self.page = Page::Result;
                self.result_scroll = 0;
                self.result = ResultState {
                    output: Some(output),
                    error: None,
                    batch: None,
                };
            }
            TaskEvent::BatchFinished(summary) => {
                self.clear_overwrite_prompts();
                self.cancellation = None;
                self.page = Page::Result;
                self.result_scroll = 0;
                self.result = ResultState {
                    output: None,
                    error: None,
                    batch: Some(summary),
                };
            }
            TaskEvent::Failed(error) => {
                self.clear_overwrite_prompts();
                self.cancellation = None;
                self.page = Page::Result;
                self.result_scroll = 0;
                self.result = ResultState {
                    output: None,
                    error: Some(error),
                    batch: None,
                };
            }
            TaskEvent::Cancelled => {
                self.clear_overwrite_prompts();
                self.cancellation = None;
                self.page = Page::Result;
                self.result_scroll = 0;
                self.result = ResultState {
                    output: None,
                    error: Some("任务已取消。".into()),
                    batch: None,
                };
            }
            TaskEvent::ConfigReloaded(config) => {
                let config = *config;
                self.source_language = config.source_language.clone();
                self.target_language = config.target_language.clone();
                self.config = config;
                self.status_message = Some("已重新加载 .env 设置。".into());
            }
            TaskEvent::ConfigReloadFailed(error) => self.status_message = Some(error),
        }
        Vec::new()
    }

    pub const fn source_mode_label(&self) -> &'static str {
        match self.source_mode {
            SourceMode::Auto => "自动（默认文本字幕轨，否则语音识别）",
            SourceMode::Embedded => "指定内嵌字幕轨",
            SourceMode::External => "外部字幕文件",
            SourceMode::Stt => "语音识别（STT）",
        }
    }

    pub const fn output_mode_label(&self) -> &'static str {
        match self.output_mode {
            SubtitleOutputMode::Translated => "仅译文",
            SubtitleOutputMode::Bilingual => "双语对照（原文在前）",
            SubtitleOutputMode::Original => "仅原文（跳过翻译）",
        }
    }

    pub fn selected_track_label(&self) -> String {
        self.selected_track
            .and_then(|index| self.tracks.track(index))
            .map_or_else(
                || "自动 / 尚未探测".into(),
                super::media::model::SubtitleTrack::display_label,
            )
    }

    /// A view-model value calculated from current state; rendering does not
    /// probe files, construct providers, or run business commands.
    pub fn output_preview(&self) -> String {
        let video =
            (!self.video_path.trim().is_empty()).then(|| PathBuf::from(self.video_path.trim()));
        let external = (!self.external_subtitle_path.trim().is_empty())
            .then(|| PathBuf::from(self.external_subtitle_path.trim()));
        let format = match self.source_mode {
            SourceMode::Stt => SubtitleFormat::Srt,
            SourceMode::External => external
                .as_deref()
                .and_then(SubtitleFormat::from_path)
                .unwrap_or(SubtitleFormat::Srt),
            SourceMode::Embedded => self
                .selected_track
                .and_then(|index| self.tracks.track(index))
                .and_then(super::media::model::SubtitleTrack::format)
                .unwrap_or(SubtitleFormat::Srt),
            SourceMode::Auto => self
                .tracks
                .auto_track()
                .and_then(super::media::model::SubtitleTrack::format)
                .unwrap_or(SubtitleFormat::Srt),
        };
        build_output_path(
            video.as_deref(),
            if self.source_mode == SourceMode::External {
                external.as_deref()
            } else {
                None
            },
            &self.target_language,
            format,
        )
        .map_or_else(
            |_| "<请设置视频或外部字幕路径>".into(),
            |path| path.display().to_string(),
        )
    }

    pub const fn processing_scroll(&self) -> u16 {
        self.processing_scroll
    }

    pub const fn result_scroll(&self) -> u16 {
        self.result_scroll
    }

    fn result_has_details(&self) -> bool {
        self.result.error.is_some()
            || self
                .result
                .batch
                .as_ref()
                .is_some_and(|summary| !summary.failed.is_empty() || !summary.skipped.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    #[test]
    fn start_is_a_command_not_render_side_effect() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.video_path = "/tmp/movie.mkv".into();
        app.home_field = HomeField::Start;
        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(commands.as_slice(), [Command::Start { .. }]));
        assert_eq!(app.page, Page::Processing);
    }

    #[test]
    fn output_mode_cycles_and_is_attached_to_the_pipeline_job() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.video_path = "/tmp/movie.mkv".into();
        app.home_field = HomeField::Output;

        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert!(commands.is_empty());
        assert_eq!(app.output_mode, SubtitleOutputMode::Bilingual);
        assert!(app.output_preview().ends_with("movie.zh-CN.srt"));

        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.output_mode, SubtitleOutputMode::Original);
        assert!(app.output_preview().ends_with("movie.zh-CN.srt"));

        app.home_field = HomeField::Start;
        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let [Command::Start { job, .. }] = commands.as_slice() else {
            panic!("expected a start command");
        };
        assert_eq!(job.output_mode, SubtitleOutputMode::Original);
    }

    #[test]
    fn overwrite_prompt_sends_the_selected_decision() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        let (response, mut responses) = unbounded_channel();
        let output = PathBuf::from("movie.zh-CN.srt");
        app.update(Action::Task(Box::new(TaskEvent::OverwriteRequested {
            output: output.clone(),
            response,
        })));

        assert_eq!(
            app.overwrite_prompt.as_ref().map(|prompt| &prompt.output),
            Some(&output)
        );
        assert!(
            app.overwrite_prompt
                .as_ref()
                .is_some_and(|prompt| !prompt.batch)
        );
        app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
        )));

        assert_eq!(responses.try_recv(), Ok(true));
        assert!(app.overwrite_prompt.is_none());
    }

    #[test]
    fn batch_overwrite_prompt_applies_to_all_existing_outputs() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        let (response, mut responses) = unbounded_channel();
        app.update(Action::Task(Box::new(TaskEvent::BatchOverwriteRequested {
            output: PathBuf::from("movie.zh-CN.srt"),
            response,
        })));

        assert!(
            app.overwrite_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.batch)
        );
        app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        )));

        assert_eq!(responses.try_recv(), Ok(false));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.contains("本批量任务"))
        );
    }

    #[test]
    fn processing_c_key_issues_a_cancellation_command() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.video_path = "/tmp/movie.mkv".into();
        app.home_field = HomeField::Start;
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        )));
        assert!(matches!(commands.as_slice(), [Command::Cancel(_)]));
    }

    #[test]
    fn path_fields_accept_global_shortcut_letters_as_text() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());

        for character in ['t', 's'] {
            let commands = app.update(Action::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
            assert!(commands.is_empty());
        }
        assert_eq!(app.page, Page::Home);
        assert_eq!(app.video_path, "ts");

        app.home_field = HomeField::ExternalSubtitle;
        for character in ['s', 't'] {
            let commands = app.update(Action::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
            assert!(commands.is_empty());
        }
        assert_eq!(app.page, Page::Home);
        assert_eq!(app.external_subtitle_path, "st");
    }

    #[test]
    fn global_home_shortcuts_remain_available_outside_text_fields() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.home_field = HomeField::Source;

        let settings = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::NONE,
        )));
        assert!(settings.is_empty());
        assert_eq!(app.page, Page::Settings);

        app.page = Page::Home;
        let tracks = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::NONE,
        )));
        assert!(tracks.is_empty());
        assert_eq!(app.page, Page::Tracks);
    }

    #[test]
    fn discovered_videos_open_a_picker_and_populate_the_video_path() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.set_video_candidates(vec![
            PathBuf::from("first.mp4"),
            PathBuf::from("second.mkv"),
        ]);
        assert_eq!(app.page, Page::Videos);

        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        )));
        let selected = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(selected.is_empty());
        assert_eq!(app.page, Page::Home);
        assert_eq!(app.video_path, "second.mkv");

        app.home_field = HomeField::Video;
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.page, Page::Videos);
    }

    #[test]
    fn discovered_videos_can_start_a_batch() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.set_video_candidates(vec![
            PathBuf::from("first.mp4"),
            PathBuf::from("second.mkv"),
        ]);

        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::NONE,
        )));

        let [Command::StartBatch { job, .. }] = commands.as_slice() else {
            panic!("expected a batch start command");
        };
        assert_eq!(
            job.videos,
            vec![PathBuf::from("first.mp4"), PathBuf::from("second.mkv")]
        );
        assert_eq!(job.subtitle_input, BatchSubtitleInput::Auto);
        assert_eq!(app.page, Page::Processing);
    }

    #[test]
    fn batch_rejects_sources_without_a_per_video_mapping() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.set_video_candidates(vec![
            PathBuf::from("first.mp4"),
            PathBuf::from("second.mkv"),
        ]);
        app.source_mode = SourceMode::External;

        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::NONE,
        )));

        assert!(commands.is_empty());
        assert_eq!(app.page, Page::Videos);
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.contains("不支持一个外部字幕路径"))
        );
    }

    #[test]
    fn batch_video_events_update_each_active_file_progress() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.page = Page::Processing;
        let first = PathBuf::from("first.mkv");
        let second = PathBuf::from("second.mkv");

        app.update(Action::Task(Box::new(TaskEvent::BatchVideoEvent {
            current: 1,
            total: 3,
            video: first.clone(),
            event: Box::new(TaskEvent::TranslationProgress {
                completed: 2,
                total: 5,
                request: 1,
            }),
        })));
        app.update(Action::Task(Box::new(TaskEvent::BatchVideoEvent {
            current: 2,
            total: 3,
            video: second.clone(),
            event: Box::new(TaskEvent::TranslationProgress {
                completed: 4,
                total: 10,
                request: 1,
            }),
        })));

        let batch = app.processing.batch.as_ref().unwrap();
        assert_eq!(batch.total, 3);
        assert_eq!(batch.active.len(), 2);
        assert_eq!(batch.active[&1].video, first);
        assert_eq!(batch.active[&1].completed, 2);
        assert_eq!(batch.active[&1].total, Some(5));
        assert_eq!(batch.active[&2].video, second);
        assert_eq!(batch.active[&2].completed, 4);
        assert_eq!(batch.active[&2].total, Some(10));

        app.update(Action::Task(Box::new(TaskEvent::BatchVideoSkipped {
            current: 1,
            total: 3,
            video: first,
        })));
        let batch = app.processing.batch.as_ref().unwrap();
        assert_eq!(batch.active.len(), 1);
        assert_eq!(batch.skipped, 1);
    }

    #[test]
    fn processing_page_scrolls_active_batch_files() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.page = Page::Processing;
        app.processing.batch = Some(BatchProcessingState::default());

        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.processing_scroll(), 10);

        let _ = app.update(Action::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(app.processing_scroll(), 9);
    }

    #[test]
    fn batch_events_produce_a_summary_result() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.page = Page::Processing;

        let started = app.update(Action::Task(Box::new(TaskEvent::BatchStarted { total: 2 })));
        assert!(started.is_empty());
        assert_eq!(
            app.processing.batch.as_ref().map(|batch| batch.total),
            Some(2)
        );

        let summary = BatchSummary {
            total: 2,
            succeeded: vec![PathBuf::from("first.zh-CN.srt")],
            skipped: Vec::new(),
            failed: vec![crate::event::BatchFailure {
                video: PathBuf::from("second.mkv"),
                error: "fixture failure".into(),
            }],
        };
        let finished = app.update(Action::Task(Box::new(TaskEvent::BatchFinished(
            summary.clone(),
        ))));

        assert!(finished.is_empty());
        assert_eq!(app.page, Page::Result);
        assert_eq!(app.result.batch, Some(summary));
    }

    #[test]
    fn a_single_discovered_video_prefills_the_video_path() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.set_video_candidates(vec![PathBuf::from("only.webm")]);

        assert_eq!(app.page, Page::Home);
        assert_eq!(app.video_path, "only.webm");
    }

    #[test]
    fn result_page_scrolls_long_errors() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.page = Page::Result;
        app.result.error = Some("long provider error".into());

        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.result_scroll(), 10);

        let _ = app.update(Action::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(app.result_scroll(), 9);
    }
}
