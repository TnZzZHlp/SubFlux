use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::{App, HomeField};

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let selected = |field| {
        if app.home_field == field {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };
    let row = |field, label: &str, value: String| {
        let marker = if app.home_field == field {
            "› "
        } else {
            "  "
        };
        Line::from(vec![
            Span::styled(marker, selected(field)),
            Span::styled(format!("{label}："), selected(field)),
            Span::raw(value),
        ])
    };
    let video = if app.video_path.is_empty() {
        "<请输入视频路径>".into()
    } else {
        app.video_path.clone()
    };
    let external = if app.external_subtitle_path.is_empty() {
        "<请输入 .srt/.ass/.ssa/.vtt 字幕路径>".into()
    } else {
        app.external_subtitle_path.clone()
    };
    let lines = vec![
        row(HomeField::Video, "视频", video),
        Line::raw(""),
        row(
            HomeField::Source,
            "字幕来源",
            app.source_mode_label().into(),
        ),
        row(HomeField::ExternalSubtitle, "外部字幕", external),
        row(HomeField::Track, "字幕轨道", app.selected_track_label()),
        Line::raw(""),
        row(
            HomeField::SourceLanguage,
            "源语言",
            format!(
                "{} ({})",
                app.source_language.display_name(),
                app.source_language
            ),
        ),
        row(
            HomeField::TargetLanguage,
            "目标语言",
            format!(
                "{} ({})",
                app.target_language.display_name(),
                app.target_language
            ),
        ),
        Line::from(format!("  输出文件：{}", app.output_preview())),
        Line::raw(""),
        row(HomeField::Start, "", "[ 开始翻译 ]".into()),
        Line::raw(""),
        Line::from(
            "Tab/↑/↓：切换字段  ←/→：切换来源/语言  P：探测字幕轨  T：字幕轨  S：设置  Q：退出",
        ),
    ];
    let title = if app.tools.is_ready() {
        " 字幕翻译器 — 首页 "
    } else {
        " 字幕翻译器 — 缺少所需工具 "
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}
