use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, InputMode, Page, ProbeStatus, ReloadStatus};

use super::pages;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LayoutMode {
    Full,
    Compact,
    Tiny,
}

impl LayoutMode {
    const fn from_area(area: Rect) -> Self {
        if area.width >= 80 && area.height >= 24 {
            Self::Full
        } else if area.width >= 42 && area.height >= 12 {
            Self::Compact
        } else {
            Self::Tiny
        }
    }
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let mode = LayoutMode::from_area(area);
    frame.render_widget(Clear, area);

    if mode == LayoutMode::Tiny {
        render_tiny(frame, app, area);
    } else {
        let areas = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);
        render_header(frame, app, areas[0]);
        match app.page {
            Page::Home => pages::home::render(frame, app, areas[1], mode),
            Page::Videos => pages::videos::render(frame, app, areas[1], mode),
            Page::Settings => pages::settings::render(frame, app, areas[1], mode),
            Page::Tracks => pages::tracks::render(frame, app, areas[1], mode),
            Page::Processing => pages::processing::render(frame, app, areas[1], mode),
            Page::Result => pages::result::render(frame, app, areas[1], mode),
        }
        render_footer(frame, app, areas[2], mode);
    }

    if app.overwrite_prompt.is_some() {
        render_overwrite_prompt(frame, app, area, mode);
    }
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let tools = if app.tools.is_ready() {
        Span::styled("媒体工具可用", Style::default().fg(Color::Green))
    } else {
        Span::styled("媒体工具不可用", Style::default().fg(Color::Red))
    };
    let line = Line::from(vec![
        Span::styled(
            " SubFlux ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("/ {}  ", page_label(app.page))),
        tools,
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let width = usize::from(area.width.saturating_sub(2));
    let mut status = status_text(app);
    if mode == LayoutMode::Compact {
        status = format!("建议终端至少 80×24  |  {status}");
    }
    let lines = vec![
        Line::from(Span::styled(
            truncate_text(&status, width),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(truncate_text(help_text(app), width)),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::TOP)),
        area,
    );
}

fn render_tiny(frame: &mut Frame, app: &App, area: Rect) {
    let width = usize::from(area.width.saturating_sub(2));
    let lines = vec![
        Line::from(Span::styled(
            format!("SubFlux / {}", page_label(app.page)),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(truncate_text(&status_text(app), width)),
        Line::from(truncate_text("终端过小，建议至少 80×24。", width)),
        Line::from(truncate_text(help_text(app), width)),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_overwrite_prompt(frame: &mut Frame, app: &App, outer: Rect, mode: LayoutMode) {
    let Some(prompt) = &app.overwrite_prompt else {
        return;
    };
    let area = centered_rect(outer, if mode == LayoutMode::Tiny { 6 } else { 8 }, 80);
    let path_width = usize::from(area.width.saturating_sub(4));
    let path = truncate_text(&prompt.output.display().to_string(), path_width);
    let lines = if area.height <= 5 {
        vec![Line::from("Y：覆盖  N：跳过")]
    } else if mode == LayoutMode::Tiny {
        vec![
            Line::from(if prompt.batch {
                "批量输出已存在"
            } else {
                "输出已存在"
            }),
            Line::from(path),
            Line::from("Y/Enter：覆盖"),
            Line::from("N/Esc：跳过"),
        ]
    } else if prompt.batch {
        vec![
            Line::from("批量任务发现已有输出："),
            Line::from(path),
            Line::raw(""),
            Line::from("Y/Enter：覆盖本批所有已有输出"),
            Line::from("N/Esc：跳过本批所有已有输出"),
        ]
    } else {
        vec![
            Line::from("输出文件已存在："),
            Line::from(path),
            Line::raw(""),
            Line::from("Y/Enter：覆盖    N/Esc：跳过"),
        ]
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" 覆盖确认 ").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

const fn page_label(page: Page) -> &'static str {
    match page {
        Page::Home => "任务设置",
        Page::Videos => "视频选择",
        Page::Settings => "配置诊断",
        Page::Tracks => "字幕轨道",
        Page::Processing => "处理进度",
        Page::Result => "处理结果",
    }
}

fn status_text(app: &App) -> String {
    match app.page {
        Page::Home | Page::Tracks => match &app.probe_status {
            ProbeStatus::Idle => app.status_message.clone().unwrap_or_else(|| "就绪".into()),
            ProbeStatus::Loading => "正在探测字幕轨…".into(),
            ProbeStatus::Ready(count) => format!("探测完成：找到 {count} 条字幕轨"),
            ProbeStatus::Failed(error) => format!("探测失败：{error}"),
        },
        Page::Settings => match &app.reload_status {
            ReloadStatus::Idle => "配置来自当前目录的 .env 和系统环境变量".into(),
            ReloadStatus::Loading => "正在重新加载配置…".into(),
            ReloadStatus::Succeeded => "配置已重新加载".into(),
            ReloadStatus::Failed(error) => format!("重新加载失败：{error}"),
        },
        Page::Processing => app.processing.stage.clone(),
        Page::Result => {
            if app.result.output.is_some() {
                "处理完成".into()
            } else if let Some(summary) = &app.result.batch {
                format!(
                    "批量完成：成功 {}，跳过 {}，失败 {}",
                    summary.succeeded.len(),
                    summary.skipped.len(),
                    summary.failed.len()
                )
            } else {
                "处理未完成".into()
            }
        }
        Page::Videos => format!("已发现 {} 个视频", app.video_candidates.len()),
    }
}

fn help_text(app: &App) -> &'static str {
    match app.page {
        Page::Home if app.input_mode == InputMode::Editing => {
            "Enter/Esc 完成编辑  ←/→ 移动  Home/End  Backspace/Delete"
        }
        Page::Home => "Tab/↑/↓ 导航  Enter 选择  ←/→ 调整  V 视频  P 探测  T 轨道  S 设置  Q 退出",
        Page::Videos => "↑/↓ 选择  Home/End  PageUp/PageDown  Enter 单选  B 批量  Esc 返回",
        Page::Settings => "R 重新加载  Esc 返回  Q 退出",
        Page::Tracks => "↑/↓ 选择  Home/End  Enter 确认  A 自动  X 语音识别  P 探测  Esc 返回",
        Page::Processing => "C/Esc 取消  ↑/↓、PageUp/PageDown 查看批量任务",
        Page::Result
            if app
                .result
                .batch
                .as_ref()
                .is_some_and(|batch| !batch.failed.is_empty()) =>
        {
            "↑/↓ 选择失败项  PageUp/PageDown 滚动详情  R 重试  Enter/Esc 返回"
        }
        Page::Result => "↑/↓、PageUp/PageDown 滚动  Enter/Esc 返回  Q 退出",
    }
}

fn centered_rect(area: Rect, height: u16, max_width: u16) -> Rect {
    let width = area.width.saturating_sub(2).min(max_width);
    let height = area.height.min(height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

pub(crate) fn truncate_text(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }

    let ellipsis = '…';
    let ellipsis_width = ellipsis.width().unwrap_or_default();
    if max_width < ellipsis_width {
        return String::new();
    }
    let available_width = max_width - ellipsis_width;
    let mut truncated = String::new();
    let mut used_width = 0;
    for character in text.chars() {
        let character_width = character.width().unwrap_or_default();
        if used_width + character_width > available_width {
            break;
        }
        truncated.push(character);
        used_width += character_width;
    }
    truncated.push(ellipsis);
    truncated
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, buffer::CellWidth};
    use tokio::sync::mpsc::unbounded_channel;

    use super::{render, truncate_text};
    use crate::{
        action::Action,
        app::{App, Page, ProcessingState, ResultState},
        config::Config,
        event::{BatchFailure, BatchSummary, TaskEvent},
        media::{MediaProbe, SubtitleTrack, SubtitleTrackKind, ToolStatus, TrackIndex},
    };

    fn test_app(values: impl IntoIterator<Item = (String, String)>) -> App {
        let values = values.into_iter().collect::<HashMap<_, _>>();
        App::new(Config::from_map(&values).unwrap(), ToolStatus::default())
    }

    fn render_text(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            let mut skip = 0_u16;
            for x in 0..width {
                let cell = buffer.cell((x, y)).unwrap();
                if skip == 0 {
                    text.push_str(cell.symbol());
                }
                skip = skip.max(cell.cell_width()).saturating_sub(1);
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn truncation_preserves_chinese_character_boundaries() {
        assert_eq!(truncate_text("视频字幕翻译器", 5), "视频…");
    }

    #[test]
    fn full_home_shows_the_guided_flow_and_one_primary_action() {
        let mut app = test_app(HashMap::new());
        app.video_path = "/media/movie.mkv".into();

        let text = render_text(&app, 80, 24);

        assert!(text.contains("处理模式"));
        assert!(text.contains("1  输入"));
        assert!(text.contains("2  字幕"));
        assert!(text.contains("3  语言"));
        assert!(text.contains("4  输出"));
        assert_eq!(text.matches("开始处理单个文件").count(), 1);
        assert!(!text.contains("外部字幕："));
    }

    #[test]
    fn settings_never_render_the_raw_api_key() {
        let mut app = test_app(HashMap::from([(
            "SUBFLUX_TRANSLATOR_API_KEY".into(),
            "sensitive-secret".into(),
        )]));
        app.page = Page::Settings;

        let text = render_text(&app, 80, 24);

        assert!(text.contains("sens****cret"));
        assert!(!text.contains("sensitive-secret"));
    }

    #[test]
    fn long_video_list_keeps_the_selected_item_visible() {
        let mut app = test_app(HashMap::new());
        app.page = Page::Videos;
        app.video_candidates = (0..30)
            .map(|index| PathBuf::from(format!("video-{index:02}.mkv")))
            .collect();
        app.video_cursor = 29;

        let text = render_text(&app, 80, 24);

        assert!(text.contains("video-29.mkv"));
        assert!(!text.contains("video-00.mkv"));
    }

    #[test]
    fn long_track_list_keeps_the_selected_image_track_visible() {
        let mut app = test_app(HashMap::new());
        app.page = Page::Tracks;
        app.video_path = "movie.mkv".into();
        app.tracks = MediaProbe {
            has_audio: true,
            subtitle_tracks: (0..20)
                .map(|index| SubtitleTrack {
                    index: TrackIndex(index),
                    codec: "hdmv_pgs_subtitle".into(),
                    language: Some("zh".into()),
                    title: None,
                    default: false,
                    forced: false,
                    kind: SubtitleTrackKind::Image,
                })
                .collect(),
        };
        app.track_cursor = 19;

        let text = render_text(&app, 80, 24);

        assert!(text.contains("#19"));
        assert!(text.contains("图像字幕"));
        assert!(!text.contains("#0  "));
    }

    #[test]
    fn unknown_single_progress_has_no_fake_percentage_or_error_count() {
        let mut app = test_app(HashMap::new());
        app.page = Page::Processing;
        app.processing = ProcessingState {
            subject: Some(PathBuf::from("movie.mkv")),
            ..ProcessingState::default()
        };

        let text = render_text(&app, 80, 24);

        assert!(text.contains("等待进度数据"));
        assert!(text.contains("C 或 Esc"));
        assert!(!text.contains("错误次数"));
        assert!(!text.contains("0%"));
    }

    #[test]
    fn batch_result_separates_failure_selection_and_detail() {
        let mut app = test_app(HashMap::new());
        app.page = Page::Result;
        app.result = ResultState {
            batch: Some(BatchSummary {
                total: 2,
                succeeded: Vec::new(),
                skipped: Vec::new(),
                failed: vec![
                    BatchFailure {
                        video: PathBuf::from("first.mkv"),
                        error: "first error".into(),
                    },
                    BatchFailure {
                        video: PathBuf::from("second.mkv"),
                        error: "selected detail".into(),
                    },
                ],
            }),
            failed_cursor: 1,
            ..ResultState::default()
        };

        let text = render_text(&app, 80, 24);

        assert!(text.contains("second.mkv"));
        assert!(text.contains("selected detail"));
        assert!(text.contains("失败文件 2/2"));
    }

    #[test]
    fn compact_batch_result_scrolls_wrapped_single_line_errors() {
        let mut app = test_app(HashMap::new());
        app.page = Page::Result;
        app.result.batch = Some(BatchSummary {
            total: 1,
            succeeded: Vec::new(),
            skipped: Vec::new(),
            failed: vec![BatchFailure {
                video: PathBuf::from("movie.mkv"),
                error: format!("{}after-scroll", "x".repeat(650)),
            }],
        });
        let _ = app.update(Action::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        )));

        let text = render_text(&app, 79, 23);

        assert!(text.contains("after-scroll"));
    }

    #[test]
    fn compact_and_tiny_layouts_keep_essential_feedback() {
        let mut app = test_app(HashMap::new());
        let compact = render_text(&app, 79, 23);
        assert!(compact.contains("建议终端至少 80×24"));

        app.page = Page::Processing;
        app.processing.stage = "正在翻译".into();
        let tiny = render_text(&app, 20, 6);
        assert!(tiny.contains("正在翻译"));
        assert!(tiny.contains("取消"));

        let _ = render_text(&app, 5, 2);
    }

    #[test]
    fn overwrite_controls_remain_visible_in_a_tiny_terminal() {
        let mut app = test_app(HashMap::new());
        let (response, _responses) = unbounded_channel();
        let _ = app.update(Action::Task(Box::new(TaskEvent::OverwriteRequested {
            output: PathBuf::from("movie.zh-CN.srt"),
            response,
        })));

        let text = render_text(&app, 20, 6);
        assert!(text.contains("Y/Enter"));
        assert!(text.contains("N/Esc"));

        let boundary = render_text(&app, 20, 5);
        assert!(boundary.contains('Y'));
        assert!(boundary.contains('N'));
    }
}
