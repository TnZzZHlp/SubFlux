use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, HomeField, HomeMode, InputMode};

use super::{super::ui::LayoutMode, visible_range};
use crate::tui::ui::truncate_text;

pub(crate) fn render(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let inner_width = usize::from(area.width.saturating_sub(2));
    let (lines, cursor) = if mode == LayoutMode::Full {
        full_lines(app, inner_width)
    } else {
        compact_lines(app, inner_width, usize::from(area.height.saturating_sub(2)))
    };
    let title = if app.tools.is_ready() {
        " 新建字幕任务 "
    } else {
        " 新建字幕任务 / 媒体工具不可用 "
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
    if let Some((column, row)) = cursor {
        frame.set_cursor_position((
            area.x
                .saturating_add(1)
                .saturating_add(column)
                .min(area.right().saturating_sub(2)),
            area.y
                .saturating_add(1)
                .saturating_add(row)
                .min(area.bottom().saturating_sub(2)),
        ));
    }
}

fn full_lines(app: &App, width: usize) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines = Vec::new();
    let mut cursor = None;
    push_field(&mut lines, &mut cursor, app, HomeField::Mode, width);
    push_section(&mut lines, "1  输入");
    push_field(&mut lines, &mut cursor, app, HomeField::Video, width);
    push_section(&mut lines, "2  字幕");
    push_field(&mut lines, &mut cursor, app, HomeField::Source, width);
    if app.source_mode == crate::app::SourceMode::Embedded {
        push_field(&mut lines, &mut cursor, app, HomeField::Track, width);
    } else if app.source_mode == crate::app::SourceMode::External {
        push_field(
            &mut lines,
            &mut cursor,
            app,
            HomeField::ExternalSubtitle,
            width,
        );
    }
    push_section(&mut lines, "3  语言");
    push_field(
        &mut lines,
        &mut cursor,
        app,
        HomeField::SourceLanguage,
        width,
    );
    push_field(
        &mut lines,
        &mut cursor,
        app,
        HomeField::TargetLanguage,
        width,
    );
    push_section(&mut lines, "4  输出");
    push_field(&mut lines, &mut cursor, app, HomeField::Output, width);
    let output_preview = if app.home_mode == HomeMode::Batch {
        format!("每个视频旁生成 <文件名>.{}.字幕格式", app.target_language)
    } else {
        app.output_preview()
    };
    lines.push(Line::from(vec![
        Span::raw("   输出文件："),
        Span::styled(
            truncate_text(&output_preview, width.saturating_sub(12)),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::raw(""));
    push_field(&mut lines, &mut cursor, app, HomeField::Start, width);
    (lines, cursor)
}

fn compact_lines(
    app: &App,
    width: usize,
    capacity: usize,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let fields = app.visible_home_fields();
    let selected = fields
        .iter()
        .position(|field| *field == app.home_field)
        .unwrap_or(0);
    let range = visible_range(selected, fields.len(), capacity);
    let mut lines = Vec::new();
    let mut cursor = None;
    for field in &fields[range] {
        push_field(&mut lines, &mut cursor, app, *field, width);
    }
    (lines, cursor)
}

fn push_section(lines: &mut Vec<Line<'static>>, title: &str) {
    lines.push(Line::from(Span::styled(
        title.to_owned(),
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    )));
}

fn push_field(
    lines: &mut Vec<Line<'static>>,
    cursor: &mut Option<(u16, u16)>,
    app: &App,
    field: HomeField,
    width: usize,
) {
    let active = app.home_field == field;
    let style = if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let label = field_label(field);
    let prefix = format!("{} {label}：", if active { "›" } else { " " });
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    let value_width = width.saturating_sub(prefix_width);
    let raw_value = field_value(app, field);
    let editing = active
        && app.input_mode == InputMode::Editing
        && matches!(field, HomeField::Video | HomeField::ExternalSubtitle);
    let (value, cursor_column) = if editing {
        let (value, column) = editable_window(&raw_value, app.text_cursor, value_width);
        (value, Some(prefix_width.saturating_add(column)))
    } else {
        (truncate_text(&raw_value, value_width), None)
    };
    let row = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    lines.push(Line::from(vec![
        Span::styled(prefix, style),
        Span::styled(value, if editing { style } else { Style::default() }),
    ]));
    if let Some(column) = cursor_column {
        *cursor = Some((u16::try_from(column).unwrap_or(u16::MAX), row));
    }
}

const fn field_label(field: HomeField) -> &'static str {
    match field {
        HomeField::Mode => "处理模式",
        HomeField::Video => "视频输入",
        HomeField::Source => "字幕来源",
        HomeField::ExternalSubtitle => "外部字幕",
        HomeField::Track => "字幕轨道",
        HomeField::SourceLanguage => "源语言",
        HomeField::TargetLanguage => "目标语言",
        HomeField::Output => "输出类型",
        HomeField::Start => "主操作",
    }
}

fn field_value(app: &App, field: HomeField) -> String {
    match field {
        HomeField::Mode => match app.home_mode {
            HomeMode::Single => "[单文件]  批量".into(),
            HomeMode::Batch => "单文件  [批量]".into(),
        },
        HomeField::Video if app.home_mode == HomeMode::Batch => {
            format!("已发现 {} 个视频（Enter 查看）", app.video_candidates.len())
        }
        HomeField::Video => {
            if app.video_path.is_empty() {
                "<Enter 开始输入路径>".into()
            } else {
                app.video_path.clone()
            }
        }
        HomeField::Source => app.source_mode_label().into(),
        HomeField::ExternalSubtitle => {
            if app.external_subtitle_path.is_empty() {
                "<Enter 开始输入字幕路径>".into()
            } else {
                app.external_subtitle_path.clone()
            }
        }
        HomeField::Track => app.selected_track_label(),
        HomeField::SourceLanguage => format!(
            "{} ({})",
            app.source_language.display_name(),
            app.source_language
        ),
        HomeField::TargetLanguage => format!(
            "{} ({})",
            app.target_language.display_name(),
            app.target_language
        ),
        HomeField::Output => app.output_mode_label().into(),
        HomeField::Start if !app.tools.is_ready() => "[ 无法开始：媒体工具不可用 ]".into(),
        HomeField::Start if app.home_mode == HomeMode::Batch => {
            format!("[ 开始批量处理 {} 个视频 ]", app.video_candidates.len())
        }
        HomeField::Start => "[ 开始处理单个文件 ]".into(),
    }
}

fn editable_window(value: &str, cursor: usize, width: usize) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let characters: Vec<char> = value.chars().collect();
    let cursor = cursor.min(characters.len());
    let mut start = 0;
    while display_width(&characters[start..cursor]) >= width && start < cursor {
        start += 1;
    }
    let mut output = String::new();
    let mut used = if start > 0 {
        output.push('…');
        1
    } else {
        0
    };
    for character in &characters[start..] {
        let character_width = character.width().unwrap_or_default();
        if used + character_width >= width {
            break;
        }
        output.push(*character);
        used += character_width;
    }
    if output.is_empty() {
        output.push(' ');
    }
    let cursor_column = usize::from(start > 0) + display_width(&characters[start..cursor]);
    (output, cursor_column.min(width.saturating_sub(1)))
}

fn display_width(characters: &[char]) -> usize {
    characters
        .iter()
        .map(|character| character.width().unwrap_or_default())
        .sum()
}
