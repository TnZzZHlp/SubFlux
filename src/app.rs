use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::{
    action::Action,
    config::{Config, LanguageCode},
    event::{BatchSummary, CheckpointPhase, TaskEvent},
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
pub enum HomeMode {
    Single,
    Batch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMode {
    Navigate,
    Editing,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ProbeStatus {
    #[default]
    Idle,
    Loading,
    Ready(usize),
    Failed(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ReloadStatus {
    #[default]
    Idle,
    Loading,
    Succeeded,
    Failed(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeField {
    Mode,
    Video,
    Source,
    ExternalSubtitle,
    Track,
    SourceLanguage,
    TargetLanguage,
    Output,
    Start,
}

#[derive(Clone, Debug)]
pub struct ProcessingState {
    pub subject: Option<PathBuf>,
    pub stage: String,
    pub completed: usize,
    pub total: Option<usize>,
    pub request: Option<usize>,
    pub batch: Option<BatchProcessingState>,
}

impl Default for ProcessingState {
    fn default() -> Self {
        Self {
            subject: None,
            stage: "准备中…".into(),
            completed: 0,
            total: None,
            request: None,
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
    /// Original settings retained only while its completed batch result is visible.
    pub batch_job: Option<BatchJob>,
    pub failed_cursor: usize,
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
    pub home_mode: HomeMode,
    pub input_mode: InputMode,
    pub home_field: HomeField,
    pub text_cursor: usize,
    pub probe_status: ProbeStatus,
    pub reload_status: ReloadStatus,
    pub processing: ProcessingState,
    pub result: ResultState,
    pub status_message: Option<String>,
    pub overwrite_prompt: Option<OverwritePrompt>,
    cancellation: Option<CancellationToken>,
    next_probe_request: u64,
    active_probe_request: Option<u64>,
    processing_scroll: u16,
    result_scroll: u16,
}

#[derive(Clone, Debug)]
pub enum Command {
    Probe {
        path: PathBuf,
        request_id: u64,
    },
    Start {
        job: Box<PipelineJob>,
        cancellation: CancellationToken,
    },
    StartBatch {
        job: Box<BatchJob>,
        cancellation: CancellationToken,
    },
    RetryBatchVideo {
        job: Box<PipelineJob>,
        failed_index: usize,
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
            home_mode: HomeMode::Single,
            input_mode: InputMode::Navigate,
            home_field: HomeField::Mode,
            text_cursor: 0,
            probe_status: ProbeStatus::Idle,
            reload_status: ReloadStatus::Idle,
            processing: ProcessingState::default(),
            result: ResultState::default(),
            status_message,
            overwrite_prompt: None,
            cancellation: None,
            next_probe_request: 0,
            active_probe_request: None,
            processing_scroll: 0,
            result_scroll: 0,
        }
    }

    pub fn update(&mut self, action: Action) -> Vec<Command> {
        match action {
            Action::Key(key) => self.handle_key(key),
            Action::Paste(text) => self.handle_paste(&text),
            Action::Task(event) => self.handle_task(*event),
            Action::Tick => Vec::new(),
        }
    }

    /// Stores recursively discovered videos for either individual selection or
    /// an explicit batch run.
    pub fn set_video_candidates(&mut self, videos: Vec<PathBuf>) {
        self.video_candidates = videos;
        self.video_cursor = 0;
        self.invalidate_probe();
        match self.video_candidates.len() {
            0 => self.status_message = Some("未找到支持的视频文件。".into()),
            1 => {
                self.home_mode = HomeMode::Single;
                self.select_video_candidate();
            }
            count => {
                self.home_mode = HomeMode::Batch;
                self.input_mode = InputMode::Navigate;
                self.home_field = HomeField::Mode;
                self.page = Page::Home;
                self.status_message = Some(format!(
                    "已发现 {count} 个视频，可开始批量处理或切换到单文件模式。"
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

    fn handle_paste(&mut self, text: &str) -> Vec<Command> {
        if self.page == Page::Home
            && self.overwrite_prompt.is_none()
            && self.input_mode == InputMode::Editing
        {
            self.insert_current_text(text);
        }
        Vec::new()
    }

    fn handle_home_key(&mut self, key: KeyEvent) -> Vec<Command> {
        if self.input_mode == InputMode::Editing {
            return self.handle_home_editing_key(key);
        }
        match key.code {
            KeyCode::Char('q' | 'Q') => vec![Command::Quit],
            KeyCode::Char('s' | 'S') => {
                self.page = Page::Settings;
                Vec::new()
            }
            KeyCode::Char('t' | 'T') if self.home_mode == HomeMode::Batch => {
                self.status_message =
                    Some("批量模式仅支持自动字幕来源或语音识别，无需选择单条字幕轨。".into());
                Vec::new()
            }
            KeyCode::Char('t' | 'T') => {
                self.page = Page::Tracks;
                Vec::new()
            }
            KeyCode::Char('v' | 'V') if !self.video_candidates.is_empty() => {
                self.page = Page::Videos;
                Vec::new()
            }
            KeyCode::Char('b' | 'B') => self.start_batch_command(),
            KeyCode::Char('p' | 'P') => self.probe_command(),
            KeyCode::Tab | KeyCode::Down => {
                self.move_home_focus(false);
                Vec::new()
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.move_home_focus(true);
                Vec::new()
            }
            KeyCode::Left => self.adjust_current(-1),
            KeyCode::Right => self.adjust_current(1),
            KeyCode::Enter => self.activate_home_field(),
            _ => Vec::new(),
        }
    }

    fn handle_home_editing_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.input_mode = InputMode::Navigate,
            KeyCode::Left => self.text_cursor = self.text_cursor.saturating_sub(1),
            KeyCode::Right => {
                self.text_cursor = self
                    .text_cursor
                    .saturating_add(1)
                    .min(self.current_text_len());
            }
            KeyCode::Home => self.text_cursor = 0,
            KeyCode::End => self.text_cursor = self.current_text_len(),
            KeyCode::Backspace => self.delete_before_cursor(),
            KeyCode::Delete => self.delete_at_cursor(),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_current_text(&character.to_string());
            }
            _ => {}
        }
        Vec::new()
    }

    fn handle_settings_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('h' | 'H') => {
                self.page = Page::Home;
                Vec::new()
            }
            KeyCode::Char('r' | 'R') if self.reload_status != ReloadStatus::Loading => {
                self.reload_status = ReloadStatus::Loading;
                vec![Command::ReloadConfig]
            }
            KeyCode::Char('q' | 'Q') => vec![Command::Quit],
            _ => Vec::new(),
        }
    }

    fn handle_videos_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let maximum = self.video_candidates.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('h' | 'H') => self.page = Page::Home,
            KeyCode::Up => self.video_cursor = self.video_cursor.saturating_sub(1),
            KeyCode::Down => self.video_cursor = self.video_cursor.saturating_add(1).min(maximum),
            KeyCode::Home => self.video_cursor = 0,
            KeyCode::End => self.video_cursor = maximum,
            KeyCode::PageUp => self.video_cursor = self.video_cursor.saturating_sub(10),
            KeyCode::PageDown => {
                self.video_cursor = self.video_cursor.saturating_add(10).min(maximum);
            }
            KeyCode::Enter => self.select_video_candidate(),
            KeyCode::Char('b' | 'B') => return self.start_batch_command(),
            KeyCode::Char('q' | 'Q') => return vec![Command::Quit],
            _ => {}
        }
        Vec::new()
    }

    fn handle_tracks_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let maximum = self.tracks.subtitle_tracks.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('h' | 'H') => self.page = Page::Home,
            KeyCode::Char('a' | 'A') => {
                self.source_mode = SourceMode::Auto;
                self.page = Page::Home;
            }
            KeyCode::Char('x' | 'X') => {
                self.source_mode = SourceMode::Stt;
                self.page = Page::Home;
            }
            KeyCode::Up => self.track_cursor = self.track_cursor.saturating_sub(1),
            KeyCode::Down => self.track_cursor = self.track_cursor.saturating_add(1).min(maximum),
            KeyCode::Home => self.track_cursor = 0,
            KeyCode::End => self.track_cursor = maximum,
            KeyCode::PageUp => self.track_cursor = self.track_cursor.saturating_sub(10),
            KeyCode::PageDown => {
                self.track_cursor = self.track_cursor.saturating_add(10).min(maximum);
            }
            KeyCode::Enter if self.home_mode == HomeMode::Batch => {
                self.source_mode = SourceMode::Auto;
                self.page = Page::Home;
                self.status_message =
                    Some("批量模式不能指定单条字幕轨，已保持自动字幕来源。".into());
            }
            KeyCode::Enter => {
                if let Some(track) = self.tracks.subtitle_tracks.get(self.track_cursor) {
                    if track.is_text() {
                        self.source_mode = SourceMode::Embedded;
                        self.selected_track = Some(track.index);
                        self.page = Page::Home;
                        self.status_message = Some(format!("已选择 {}", track.display_label()));
                    } else {
                        self.status_message =
                            Some("当前字幕轨不能直接翻译，请选择语音识别模式（按 X）。".into());
                    }
                }
            }
            KeyCode::Char('p' | 'P') => return self.probe_command(),
            KeyCode::Char('q' | 'Q') => return vec![Command::Quit],
            _ => {}
        }
        Vec::new()
    }

    fn handle_processing_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Char('c' | 'C') | KeyCode::Esc => self.cancel_or_quit(),
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
            KeyCode::Up if self.has_retryable_batch_failure() => {
                self.result.failed_cursor = self.result.failed_cursor.saturating_sub(1);
                self.result_scroll = 0;
                Vec::new()
            }
            KeyCode::Down if self.has_retryable_batch_failure() => {
                let failed = self
                    .result
                    .batch
                    .as_ref()
                    .map_or(0, |summary| summary.failed.len());
                self.result.failed_cursor = self
                    .result
                    .failed_cursor
                    .saturating_add(1)
                    .min(failed.saturating_sub(1));
                self.result_scroll = 0;
                Vec::new()
            }
            KeyCode::Char('r' | 'R') => self.retry_selected_batch_video(),
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
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('h' | 'H') => {
                self.page = Page::Home;
                self.result = ResultState::default();
                self.result_scroll = 0;
                Vec::new()
            }
            KeyCode::Char('q' | 'Q') => vec![Command::Quit],
            _ => Vec::new(),
        }
    }

    fn retry_selected_batch_video(&mut self) -> Vec<Command> {
        let (failed_index, video, job) = {
            let failed_index = self.result.failed_cursor;
            let Some(summary) = self.result.batch.as_ref() else {
                return Vec::new();
            };
            let Some(failure) = summary.failed.get(failed_index) else {
                return Vec::new();
            };
            let Some(batch_job) = self.result.batch_job.as_ref() else {
                return Vec::new();
            };
            let video = failure.video.clone();
            (failed_index, video.clone(), batch_job.pipeline_job(video))
        };
        let cancellation = CancellationToken::new();
        self.processing = ProcessingState {
            subject: Some(video.clone()),
            ..ProcessingState::default()
        };
        self.processing_scroll = 0;
        self.result_scroll = 0;
        self.status_message = Some(format!("正在重试：{}…", video.display()));
        self.cancellation = Some(cancellation.clone());
        self.page = Page::Processing;
        vec![Command::RetryBatchVideo {
            job: Box::new(job),
            failed_index,
            cancellation,
        }]
    }

    fn activate_home_field(&mut self) -> Vec<Command> {
        match self.home_field {
            HomeField::Mode => {
                self.adjust_home_mode();
                Vec::new()
            }
            HomeField::Video if self.home_mode == HomeMode::Batch => {
                if self.video_candidates.is_empty() {
                    self.status_message = Some("启动时未发现可批量处理的视频。".into());
                } else {
                    self.page = Page::Videos;
                }
                Vec::new()
            }
            HomeField::Video | HomeField::ExternalSubtitle => {
                self.input_mode = InputMode::Editing;
                self.text_cursor = self.current_text_len();
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
            HomeField::Start => match self.home_mode {
                HomeMode::Single => self.start_command(),
                HomeMode::Batch => self.start_batch_command(),
            },
        }
    }

    fn select_video_candidate(&mut self) {
        let Some(path) = self.video_candidates.get(self.video_cursor).cloned() else {
            return;
        };
        let Some(path) = path.to_str() else {
            self.status_message = Some("所选视频路径不是有效的 UTF-8 文本。".into());
            return;
        };
        self.video_path = path.into();
        self.home_mode = HomeMode::Single;
        self.input_mode = InputMode::Navigate;
        self.text_cursor = self.video_path.chars().count();
        self.invalidate_probe();
        self.page = Page::Home;
        self.status_message = Some("已选择单个视频。".into());
    }

    pub fn visible_home_fields(&self) -> Vec<HomeField> {
        let mut fields = vec![HomeField::Mode, HomeField::Video, HomeField::Source];
        if self.home_mode == HomeMode::Single {
            match self.source_mode {
                SourceMode::Embedded => fields.push(HomeField::Track),
                SourceMode::External => fields.push(HomeField::ExternalSubtitle),
                SourceMode::Auto | SourceMode::Stt => {}
            }
        }
        fields.extend([
            HomeField::SourceLanguage,
            HomeField::TargetLanguage,
            HomeField::Output,
            HomeField::Start,
        ]);
        fields
    }

    fn move_home_focus(&mut self, backwards: bool) {
        let fields = self.visible_home_fields();
        let index = fields
            .iter()
            .position(|field| *field == self.home_field)
            .unwrap_or(0);
        self.home_field = if backwards {
            fields[(index + fields.len() - 1) % fields.len()]
        } else {
            fields[(index + 1) % fields.len()]
        };
    }

    fn normalize_home_focus(&mut self) {
        if !self.visible_home_fields().contains(&self.home_field) {
            self.home_field = HomeField::Source;
        }
    }

    fn adjust_home_mode(&mut self) {
        self.home_mode = match self.home_mode {
            HomeMode::Single => HomeMode::Batch,
            HomeMode::Batch => HomeMode::Single,
        };
        self.input_mode = InputMode::Navigate;
        if self.home_mode == HomeMode::Batch
            && !matches!(self.source_mode, SourceMode::Auto | SourceMode::Stt)
        {
            self.source_mode = SourceMode::Auto;
        }
        self.normalize_home_focus();
    }

    fn adjust_current(&mut self, direction: i8) -> Vec<Command> {
        match self.home_field {
            HomeField::Mode => self.adjust_home_mode(),
            HomeField::Source => self.adjust_source(direction),
            HomeField::SourceLanguage => self.cycle_language(true, direction),
            HomeField::TargetLanguage => self.cycle_language(false, direction),
            HomeField::Output => self.adjust_output_mode(direction),
            HomeField::Video
            | HomeField::ExternalSubtitle
            | HomeField::Track
            | HomeField::Start => {}
        }
        Vec::new()
    }

    fn adjust_source(&mut self, direction: i8) {
        let modes: &[SourceMode] = if self.home_mode == HomeMode::Batch {
            &[SourceMode::Auto, SourceMode::Stt]
        } else {
            &[
                SourceMode::Auto,
                SourceMode::Embedded,
                SourceMode::External,
                SourceMode::Stt,
            ]
        };
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
        self.normalize_home_focus();
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
            SubtitleOutputMode::BilingualTranslationFirst,
            SubtitleOutputMode::Bilingual,
            SubtitleOutputMode::Translated,
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

    fn current_text_len(&self) -> usize {
        match self.home_field {
            HomeField::Video => self.video_path.chars().count(),
            HomeField::ExternalSubtitle => self.external_subtitle_path.chars().count(),
            HomeField::Mode
            | HomeField::Source
            | HomeField::Track
            | HomeField::SourceLanguage
            | HomeField::TargetLanguage
            | HomeField::Output
            | HomeField::Start => 0,
        }
    }

    const fn current_text_mut(&mut self) -> Option<&mut String> {
        match self.home_field {
            HomeField::Video => Some(&mut self.video_path),
            HomeField::ExternalSubtitle => Some(&mut self.external_subtitle_path),
            HomeField::Mode
            | HomeField::Source
            | HomeField::Track
            | HomeField::SourceLanguage
            | HomeField::TargetLanguage
            | HomeField::Output
            | HomeField::Start => None,
        }
    }

    fn byte_index(value: &str, character_index: usize) -> usize {
        value
            .char_indices()
            .nth(character_index)
            .map_or(value.len(), |(index, _)| index)
    }

    fn insert_current_text(&mut self, text: &str) {
        let cursor = self.text_cursor;
        let edited_video = self.home_field == HomeField::Video;
        {
            let Some(value) = self.current_text_mut() else {
                return;
            };
            let byte_index = Self::byte_index(value, cursor);
            value.insert_str(byte_index, text);
        }
        self.text_cursor = cursor + text.chars().count();
        if edited_video {
            self.invalidate_probe();
        }
    }

    fn delete_before_cursor(&mut self) {
        if self.text_cursor == 0 {
            return;
        }
        let cursor = self.text_cursor;
        let edited_video = self.home_field == HomeField::Video;
        {
            let Some(value) = self.current_text_mut() else {
                return;
            };
            let start = Self::byte_index(value, cursor - 1);
            let end = Self::byte_index(value, cursor);
            value.replace_range(start..end, "");
        }
        self.text_cursor -= 1;
        if edited_video {
            self.invalidate_probe();
        }
    }

    fn delete_at_cursor(&mut self) {
        let cursor = self.text_cursor;
        let edited_video = self.home_field == HomeField::Video;
        {
            let Some(value) = self.current_text_mut() else {
                return;
            };
            let start = Self::byte_index(value, cursor);
            let end = Self::byte_index(value, cursor.saturating_add(1));
            if start == end {
                return;
            }
            value.replace_range(start..end, "");
        }
        if edited_video {
            self.invalidate_probe();
        }
    }

    fn invalidate_probe(&mut self) {
        self.active_probe_request = None;
        self.probe_status = ProbeStatus::Idle;
        self.tracks = MediaProbe::default();
        self.selected_track = None;
        self.track_cursor = 0;
    }

    fn probe_command(&mut self) -> Vec<Command> {
        let value = self.video_path.trim().to_owned();
        if value.is_empty() {
            let message = "请先输入视频路径，再探测字幕轨。".to_owned();
            self.probe_status = ProbeStatus::Failed(message.clone());
            self.status_message = Some(message);
            return Vec::new();
        }
        self.next_probe_request = self.next_probe_request.wrapping_add(1);
        let request_id = self.next_probe_request;
        self.active_probe_request = Some(request_id);
        self.probe_status = ProbeStatus::Loading;
        self.tracks = MediaProbe::default();
        self.selected_track = None;
        self.track_cursor = 0;
        self.status_message = None;
        vec![Command::Probe {
            path: PathBuf::from(value),
            request_id,
        }]
    }

    fn start_command(&mut self) -> Vec<Command> {
        if !self.tools.is_ready() {
            self.page = Page::Result;
            self.result = ResultState {
                error: Some(format!(
                    "所需媒体工具不可用，无法开始：{}",
                    self.tools.problems().join("; ")
                )),
                ..ResultState::default()
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
        let subject = video.clone().or_else(|| match &input {
            SubtitleInput::External(path) => Some(path.clone()),
            SubtitleInput::Auto | SubtitleInput::Embedded(_) | SubtitleInput::Stt => None,
        });
        let cancellation = CancellationToken::new();
        let job = PipelineJob {
            video,
            input,
            source_language: self.source_language.clone(),
            target_language: self.target_language.clone(),
            output_mode: self.output_mode,
            config: self.config.clone(),
        };
        self.processing = ProcessingState {
            subject,
            ..ProcessingState::default()
        };
        self.processing_scroll = 0;
        self.result = ResultState::default();
        self.result_scroll = 0;
        self.status_message = None;
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
                error: Some(format!(
                    "所需媒体工具不可用，无法开始：{}",
                    self.tools.problems().join("; ")
                )),
                ..ResultState::default()
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
        self.processing = ProcessingState {
            batch: Some(BatchProcessingState {
                total,
                ..BatchProcessingState::default()
            }),
            ..ProcessingState::default()
        };
        self.processing_scroll = 0;
        self.result = ResultState {
            batch_job: Some(job.clone()),
            ..ResultState::default()
        };
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
        video: &Path,
        event: TaskEvent,
    ) {
        match event {
            TaskEvent::Probing => {
                self.batch_file(current, total, video).stage = "正在探测媒体…".into();
            }
            TaskEvent::TracksLoaded(_) => {
                self.batch_file(current, total, video).stage = "字幕轨已加载…".into();
            }
            TaskEvent::ExtractingSubtitle => {
                self.batch_file(current, total, video).stage = "正在提取字幕…".into();
            }
            TaskEvent::ExtractingAudio => {
                self.batch_file(current, total, video).stage = "正在提取音频…".into();
            }
            TaskEvent::CheckpointResumed {
                phase,
                completed,
                total: file_total,
            } => {
                let file = self.batch_file(current, total, video);
                file.stage = match phase {
                    CheckpointPhase::Stt => "正在恢复语音识别…",
                    CheckpointPhase::Translation => "正在恢复翻译…",
                }
                .into();
                file.completed = completed;
                file.total = Some(file_total);
                file.request = None;
            }
            TaskEvent::SttStarted {
                current: completed,
                total: file_total,
            } => {
                let file = self.batch_file(current, total, video);
                file.stage = "正在进行语音识别…".into();
                file.completed = completed;
                file.total = Some(file_total);
                file.request = None;
            }
            TaskEvent::SttProgress {
                current: completed,
                total: file_total,
            } => {
                let file = self.batch_file(current, total, video);
                file.stage = "正在进行语音识别…".into();
                file.completed = completed;
                file.total = file_total;
            }
            TaskEvent::TranslationStarted { total: file_total } => {
                let file = self.batch_file(current, total, video);
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
                let file = self.batch_file(current, total, video);
                file.stage = "正在翻译…".into();
                file.completed = completed;
                file.total = Some(file_total);
                file.request = Some(request);
            }
            TaskEvent::OverwriteRequested { output, response } => {
                self.batch_file(current, total, video).stage = "等待覆盖确认…".into();
                self.overwrite_prompt = Some(OverwritePrompt {
                    output,
                    batch: false,
                    response,
                });
            }
            TaskEvent::BatchOverwriteRequested { output, response } => {
                self.batch_file(current, total, video).stage = "等待批量覆盖确认…".into();
                self.overwrite_prompt = Some(OverwritePrompt {
                    output,
                    batch: true,
                    response,
                });
            }
            TaskEvent::Writing => {
                self.batch_file(current, total, video).stage = "正在写入字幕…".into();
            }
            TaskEvent::BatchStarted { .. }
            | TaskEvent::BatchVideoStarted { .. }
            | TaskEvent::BatchVideoEvent { .. }
            | TaskEvent::BatchVideoSucceeded { .. }
            | TaskEvent::BatchVideoSkipped { .. }
            | TaskEvent::BatchVideoFailed { .. }
            | TaskEvent::BatchRetrySucceeded { .. }
            | TaskEvent::BatchRetrySkipped { .. }
            | TaskEvent::BatchRetryFailed { .. }
            | TaskEvent::BatchRetryCancelled
            | TaskEvent::ProbeSucceeded { .. }
            | TaskEvent::ProbeFailed { .. }
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
            } => self.handle_batch_video_event(current, total, &video, *event),
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
                self.status_message = Some(format!("第 {current}/{total} 个视频处理失败：{error}"));
            }
            TaskEvent::ProbeSucceeded { request_id, probe }
                if self.active_probe_request == Some(request_id) =>
            {
                let selected = probe.auto_track().map(|track| track.index);
                self.track_cursor = selected
                    .and_then(|index| {
                        probe
                            .subtitle_tracks
                            .iter()
                            .position(|track| track.index == index)
                    })
                    .unwrap_or(0);
                let count = probe.subtitle_tracks.len();
                self.active_probe_request = None;
                self.selected_track = selected;
                self.tracks = probe;
                self.probe_status = ProbeStatus::Ready(count);
                self.status_message = None;
            }
            TaskEvent::ProbeFailed { request_id, error }
                if self.active_probe_request == Some(request_id) =>
            {
                self.active_probe_request = None;
                self.probe_status = ProbeStatus::Failed(error);
            }
            TaskEvent::ProbeSucceeded { .. } | TaskEvent::ProbeFailed { .. } => {}
            TaskEvent::Probing => {
                if self.page == Page::Processing {
                    self.processing.stage = "正在探测媒体…".into();
                }
            }
            TaskEvent::TracksLoaded(_) => {
                if self.page == Page::Processing {
                    self.processing.stage = "字幕轨已加载…".into();
                }
            }
            TaskEvent::ExtractingSubtitle => self.processing.stage = "正在提取字幕…".into(),
            TaskEvent::ExtractingAudio => self.processing.stage = "正在提取音频…".into(),
            TaskEvent::CheckpointResumed {
                phase,
                completed,
                total,
            } => {
                self.processing.stage = match phase {
                    CheckpointPhase::Stt => "正在恢复语音识别…",
                    CheckpointPhase::Translation => "正在恢复翻译…",
                }
                .into();
                self.processing.completed = completed;
                self.processing.total = Some(total);
                self.processing.request = None;
            }
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
                    ..ResultState::default()
                };
            }
            TaskEvent::BatchFinished(summary) => {
                let batch_job = self.result.batch_job.take();
                self.clear_overwrite_prompts();
                self.cancellation = None;
                self.page = Page::Result;
                self.result_scroll = 0;
                self.result = ResultState {
                    batch: Some(summary),
                    batch_job,
                    ..ResultState::default()
                };
            }
            TaskEvent::BatchRetrySucceeded {
                failed_index,
                output,
            } => {
                self.finish_batch_retry();
                if let Some(summary) = self.result.batch.as_mut()
                    && failed_index < summary.failed.len()
                {
                    summary.failed.remove(failed_index);
                    summary.succeeded.push(output);
                }
                self.clamp_failed_cursor();
            }
            TaskEvent::BatchRetrySkipped { failed_index } => {
                self.finish_batch_retry();
                if let Some(summary) = self.result.batch.as_mut()
                    && failed_index < summary.failed.len()
                {
                    let failure = summary.failed.remove(failed_index);
                    summary.skipped.push(failure.video);
                }
                self.clamp_failed_cursor();
            }
            TaskEvent::BatchRetryFailed {
                failed_index,
                error,
            } => {
                self.finish_batch_retry();
                if let Some(failure) = self
                    .result
                    .batch
                    .as_mut()
                    .and_then(|summary| summary.failed.get_mut(failed_index))
                {
                    failure.error = error;
                }
                self.clamp_failed_cursor();
            }
            TaskEvent::BatchRetryCancelled => {
                self.finish_batch_retry();
                self.clamp_failed_cursor();
            }
            TaskEvent::Failed(error) => {
                self.clear_overwrite_prompts();
                self.cancellation = None;
                self.page = Page::Result;
                self.result_scroll = 0;
                self.result = ResultState {
                    error: Some(error),
                    ..ResultState::default()
                };
            }
            TaskEvent::Cancelled => {
                self.clear_overwrite_prompts();
                self.cancellation = None;
                self.page = Page::Result;
                self.result_scroll = 0;
                self.result = ResultState {
                    error: Some("任务已取消。".into()),
                    ..ResultState::default()
                };
            }
            TaskEvent::ConfigReloaded(config) => {
                let config = *config;
                self.source_language = config.source_language.clone();
                self.target_language = config.target_language.clone();
                self.config = config;
                self.reload_status = ReloadStatus::Succeeded;
                self.status_message = None;
            }
            TaskEvent::ConfigReloadFailed(error) => {
                self.reload_status = ReloadStatus::Failed(error);
            }
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
            SubtitleOutputMode::BilingualTranslationFirst => "双语对照（译文在上）",
            SubtitleOutputMode::Bilingual => "双语对照（原文在上）",
            SubtitleOutputMode::Translated => "仅译文",
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

    fn has_retryable_batch_failure(&self) -> bool {
        self.result.batch_job.is_some()
            && self
                .result
                .batch
                .as_ref()
                .is_some_and(|summary| !summary.failed.is_empty())
    }

    fn finish_batch_retry(&mut self) {
        self.clear_overwrite_prompts();
        self.cancellation = None;
        self.page = Page::Result;
        self.result_scroll = 0;
    }

    fn clamp_failed_cursor(&mut self) {
        let failures = self
            .result
            .batch
            .as_ref()
            .map_or(0, |summary| summary.failed.len());
        self.result.failed_cursor = self.result.failed_cursor.min(failures.saturating_sub(1));
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
    use crate::media::{SubtitleTrack, SubtitleTrackKind};

    fn text_probe(index: u32) -> MediaProbe {
        MediaProbe {
            subtitle_tracks: vec![SubtitleTrack {
                index: TrackIndex(index),
                codec: "subrip".into(),
                language: Some("ja".into()),
                title: None,
                default: true,
                forced: false,
                kind: SubtitleTrackKind::Text(SubtitleFormat::Srt),
            }],
        }
    }

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

        assert_eq!(
            app.output_mode,
            SubtitleOutputMode::BilingualTranslationFirst
        );
        assert_eq!(app.output_mode_label(), "双语对照（译文在上）");
        assert!(app.output_preview().ends_with("movie.zh-CN.srt"));

        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.output_mode, SubtitleOutputMode::Bilingual);
        assert_eq!(app.output_mode_label(), "双语对照（原文在上）");

        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.output_mode, SubtitleOutputMode::Translated);

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
    fn checkpoint_resume_updates_single_and_batch_progress() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.page = Page::Processing;
        app.update(Action::Task(Box::new(TaskEvent::CheckpointResumed {
            phase: CheckpointPhase::Translation,
            completed: 3,
            total: 8,
        })));

        assert_eq!(app.processing.stage, "正在恢复翻译…");
        assert_eq!(app.processing.completed, 3);
        assert_eq!(app.processing.total, Some(8));

        let video = PathBuf::from("episode.mkv");
        app.update(Action::Task(Box::new(TaskEvent::BatchVideoEvent {
            current: 1,
            total: 1,
            video: video.clone(),
            event: Box::new(TaskEvent::CheckpointResumed {
                phase: CheckpointPhase::Stt,
                completed: 2,
                total: 4,
            }),
        })));

        let file = app.processing.batch.unwrap().active.remove(&1).unwrap();
        assert_eq!(file.video, video);
        assert_eq!(file.stage, "正在恢复语音识别…");
        assert_eq!(file.completed, 2);
        assert_eq!(file.total, Some(4));
    }

    #[test]
    fn editing_mode_accepts_global_shortcut_letters_as_text() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.home_field = HomeField::Video;
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));

        for character in ['T', 's'] {
            let commands = app.update(Action::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
            assert!(commands.is_empty());
        }
        assert_eq!(app.page, Page::Home);
        assert_eq!(app.video_path, "Ts");

        let _ = app.update(Action::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        app.home_field = HomeField::ExternalSubtitle;
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        for character in ['S', 't'] {
            let _ = app.update(Action::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
        }
        assert_eq!(app.external_subtitle_path, "St");
    }

    #[test]
    fn unicode_path_editing_supports_paste_navigation_and_delete() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.home_field = HomeField::Video;
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let _ = app.update(Action::Paste("影片 test.mkv".into()));
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Home,
            KeyModifiers::NONE,
        )));
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Delete,
            KeyModifiers::NONE,
        )));
        let _ = app.update(Action::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::NONE,
        )));

        assert_eq!(app.video_path, "片 test.mk");
        assert_eq!(app.text_cursor, app.video_path.chars().count());
    }

    #[test]
    fn video_edits_clear_stale_track_state() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.tracks = text_probe(4);
        app.selected_track = Some(TrackIndex(4));
        app.track_cursor = 3;
        app.probe_status = ProbeStatus::Ready(1);
        app.home_field = HomeField::Video;
        app.input_mode = InputMode::Editing;

        let _ = app.update(Action::Paste("movie.mkv".into()));

        assert_eq!(app.tracks.subtitle_tracks, Vec::new());
        assert_eq!(app.selected_track, None);
        assert_eq!(app.track_cursor, 0);
        assert_eq!(app.probe_status, ProbeStatus::Idle);
    }

    #[test]
    fn home_focus_only_visits_fields_visible_in_the_current_mode() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.source_mode = SourceMode::External;
        app.home_field = HomeField::ExternalSubtitle;
        assert!(
            app.visible_home_fields()
                .contains(&HomeField::ExternalSubtitle)
        );

        app.adjust_home_mode();

        assert_eq!(app.home_mode, HomeMode::Batch);
        assert_eq!(app.source_mode, SourceMode::Auto);
        assert_eq!(app.home_field, HomeField::Source);
        assert!(
            !app.visible_home_fields()
                .contains(&HomeField::ExternalSubtitle)
        );
        for _ in 0..app.visible_home_fields().len() * 2 {
            let _ = app.update(Action::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
            assert!(app.visible_home_fields().contains(&app.home_field));
        }
    }

    #[test]
    fn batch_mode_cannot_enter_embedded_track_selection() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.home_mode = HomeMode::Batch;
        app.tracks = text_probe(2);

        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('T'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.page, Page::Home);
        assert_eq!(app.source_mode, SourceMode::Auto);

        app.page = Page::Tracks;
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.page, Page::Home);
        assert_eq!(app.source_mode, SourceMode::Auto);
        assert_eq!(app.selected_track, None);
    }

    #[test]
    fn only_the_latest_manual_probe_updates_tracks() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.video_path = "movie.mkv".into();

        let first = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::NONE,
        )));
        let [
            Command::Probe {
                request_id: first_id,
                ..
            },
        ] = first.as_slice()
        else {
            panic!("expected first probe command");
        };
        let first_id = *first_id;
        let second = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::NONE,
        )));
        let [
            Command::Probe {
                request_id: second_id,
                ..
            },
        ] = second.as_slice()
        else {
            panic!("expected second probe command");
        };
        let second_id = *second_id;

        let _ = app.update(Action::Task(Box::new(TaskEvent::ProbeSucceeded {
            request_id: first_id,
            probe: text_probe(1),
        })));
        assert_eq!(app.probe_status, ProbeStatus::Loading);
        assert_eq!(app.tracks.subtitle_tracks, Vec::new());

        let _ = app.update(Action::Task(Box::new(TaskEvent::ProbeSucceeded {
            request_id: second_id,
            probe: text_probe(4),
        })));
        assert_eq!(app.probe_status, ProbeStatus::Ready(1));
        assert_eq!(app.selected_track, Some(TrackIndex(4)));
        assert_eq!(app.track_cursor, 0);
    }

    #[test]
    fn manual_probe_failure_stays_on_the_current_page() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.video_path = "broken.mkv".into();
        app.page = Page::Tracks;
        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::NONE,
        )));
        let [Command::Probe { request_id, .. }] = commands.as_slice() else {
            panic!("expected probe command");
        };
        let request_id = *request_id;

        let _ = app.update(Action::Task(Box::new(TaskEvent::ProbeFailed {
            request_id,
            error: "无法读取媒体".into(),
        })));

        assert_eq!(app.page, Page::Tracks);
        assert_eq!(app.probe_status, ProbeStatus::Failed("无法读取媒体".into()));
    }

    #[test]
    fn reload_failure_preserves_the_active_config() {
        let config = Config::from_map(&HashMap::from([(
            "SUBFLUX_TRANSLATOR_MODEL".into(),
            "working-model".into(),
        )]))
        .unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.page = Page::Settings;

        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('R'),
            KeyModifiers::NONE,
        )));
        assert!(matches!(commands.as_slice(), [Command::ReloadConfig]));
        assert_eq!(app.reload_status, ReloadStatus::Loading);

        let _ = app.update(Action::Task(Box::new(TaskEvent::ConfigReloadFailed(
            "配置无效".into(),
        ))));
        assert_eq!(app.config.translator.model, "working-model");
        assert_eq!(app.reload_status, ReloadStatus::Failed("配置无效".into()));
    }

    #[test]
    fn global_home_shortcuts_remain_available_outside_text_fields() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.home_field = HomeField::Source;

        let settings = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('S'),
            KeyModifiers::NONE,
        )));
        assert!(settings.is_empty());
        assert_eq!(app.page, Page::Settings);

        app.page = Page::Home;
        let tracks = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('T'),
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
        assert_eq!(app.page, Page::Home);
        assert_eq!(app.home_mode, HomeMode::Batch);

        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('V'),
            KeyModifiers::NONE,
        )));
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

        assert_eq!(app.home_mode, HomeMode::Single);
        app.home_field = HomeField::Video;
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.input_mode, InputMode::Editing);
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
        assert_eq!(app.page, Page::Home);
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
    fn batch_result_retries_the_selected_failure_with_its_original_snapshot() {
        let config = Config::from_map(&HashMap::from([(
            "SUBFLUX_TRANSLATOR_MODEL".into(),
            "snapshot-model".into(),
        )]))
        .unwrap();
        let mut app = App::new(config, ToolStatus::default());
        app.set_video_candidates(vec![
            PathBuf::from("first.mkv"),
            PathBuf::from("second.mkv"),
        ]);
        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::NONE,
        )));
        assert!(matches!(commands.as_slice(), [Command::StartBatch { .. }]));

        app.target_language = LanguageCode::parse("en").unwrap();
        app.config.translator.model = "current-model".into();
        app.update(Action::Task(Box::new(TaskEvent::BatchFinished(
            BatchSummary {
                total: 2,
                succeeded: Vec::new(),
                skipped: Vec::new(),
                failed: vec![
                    crate::event::BatchFailure {
                        video: PathBuf::from("first.mkv"),
                        error: "first failure".into(),
                    },
                    crate::event::BatchFailure {
                        video: PathBuf::from("second.mkv"),
                        error: "second failure".into(),
                    },
                ],
            },
        ))));
        app.result_scroll = 10;
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.result_scroll(), 0);

        let commands = app.update(Action::Key(KeyEvent::new(
            KeyCode::Char('R'),
            KeyModifiers::NONE,
        )));
        let [
            Command::RetryBatchVideo {
                job, failed_index, ..
            },
        ] = commands.as_slice()
        else {
            panic!("expected a single-video retry command");
        };
        assert_eq!(*failed_index, 1);
        assert_eq!(job.video, Some(PathBuf::from("second.mkv")));
        assert!(matches!(&job.input, SubtitleInput::Auto));
        assert_eq!(job.target_language, LanguageCode::parse("zh-CN").unwrap());
        assert_eq!(job.config.translator.model, "snapshot-model");
        assert_eq!(app.page, Page::Processing);
    }

    #[test]
    fn batch_retry_events_update_only_the_selected_failure() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let mut app = App::new(config.clone(), ToolStatus::default());
        app.page = Page::Processing;
        app.result = ResultState {
            batch: Some(BatchSummary {
                total: 3,
                succeeded: Vec::new(),
                skipped: Vec::new(),
                failed: vec![
                    crate::event::BatchFailure {
                        video: PathBuf::from("first.mkv"),
                        error: "first failure".into(),
                    },
                    crate::event::BatchFailure {
                        video: PathBuf::from("second.mkv"),
                        error: "second failure".into(),
                    },
                    crate::event::BatchFailure {
                        video: PathBuf::from("third.mkv"),
                        error: "third failure".into(),
                    },
                ],
            }),
            batch_job: Some(BatchJob {
                videos: vec![
                    PathBuf::from("first.mkv"),
                    PathBuf::from("second.mkv"),
                    PathBuf::from("third.mkv"),
                ],
                subtitle_input: BatchSubtitleInput::Auto,
                source_language: LanguageCode::auto(),
                target_language: LanguageCode::parse("zh-CN").unwrap(),
                output_mode: SubtitleOutputMode::Translated,
                config,
            }),
            failed_cursor: 1,
            ..ResultState::default()
        };

        app.update(Action::Task(Box::new(TaskEvent::BatchRetrySucceeded {
            failed_index: 1,
            output: PathBuf::from("second.zh-CN.srt"),
        })));
        let summary = app.result.batch.as_ref().unwrap();
        assert_eq!(summary.succeeded, vec![PathBuf::from("second.zh-CN.srt")]);
        assert_eq!(summary.failed.len(), 2);
        assert_eq!(summary.failed[1].video, PathBuf::from("third.mkv"));

        app.page = Page::Processing;
        app.update(Action::Task(Box::new(TaskEvent::BatchRetryFailed {
            failed_index: 1,
            error: "retry failure".into(),
        })));
        assert_eq!(
            app.result.batch.as_ref().unwrap().failed[1].error,
            "retry failure"
        );

        app.page = Page::Processing;
        app.update(Action::Task(Box::new(TaskEvent::BatchRetrySkipped {
            failed_index: 0,
        })));
        let summary = app.result.batch.as_ref().unwrap();
        assert_eq!(summary.skipped, vec![PathBuf::from("first.mkv")]);
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(app.result.failed_cursor, 0);

        let failures = summary.failed.clone();
        app.page = Page::Processing;
        app.update(Action::Task(Box::new(TaskEvent::BatchRetryCancelled)));
        assert_eq!(app.result.batch.as_ref().unwrap().failed, failures);
        assert_eq!(app.page, Page::Result);
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
