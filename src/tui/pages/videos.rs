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
        Line::from(format!(
            "启动路径中找到 {} 个视频文件。",
            app.video_candidates.len()
        )),
        Line::raw(""),
    ];
    for (index, video) in app.video_candidates.iter().enumerate() {
        let active = index == app.video_cursor;
        let style = if active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(if active { "› " } else { "  " }, style),
            Span::styled(video.display().to_string(), style),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(
        "↑/↓：选择  Enter：确认单个  B：连续处理全部  Esc/H：返回首页  Q：退出",
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" 视频选择 / 批量处理 ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}
