use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::App;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = vec![
        Line::from("A：自动（默认文本字幕轨，否则语音识别）  X：使用语音识别"),
        Line::from("Enter：选择文本字幕轨。图像字幕不能直接翻译。"),
        Line::raw(""),
    ];
    if app.tracks.subtitle_tracks.is_empty() {
        lines.push(Line::from("尚未加载字幕轨。请输入视频路径后按 P 探测。"));
    } else {
        for (index, track) in app.tracks.subtitle_tracks.iter().enumerate() {
            let active = index == app.track_cursor;
            let style = if active {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(if active { "› " } else { "  " }, style),
                Span::styled(track.display_label(), style),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(
        "↑/↓：选择  Enter：确认  P：探测  Esc/H：返回首页",
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" 字幕轨道选择 ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}
