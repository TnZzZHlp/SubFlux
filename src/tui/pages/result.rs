use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::App;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let lines = app.result.output.as_ref().map_or_else(
        || {
            let error = app.result.error.as_deref().unwrap_or("未知错误");
            let mut lines = vec![
                Line::from(Span::styled("翻译未完成", Style::default().fg(Color::Red))),
                Line::raw(""),
            ];
            lines.extend(error.lines().map(|line| Line::from(line.to_owned())));
            lines.extend([
                Line::raw(""),
                Line::from("↑/↓、PageUp/PageDown：滚动错误  Enter/Esc：返回首页  Q：退出"),
            ]);
            lines
        },
        |output| {
            vec![
                Line::from(Span::styled("翻译完成", Style::default().fg(Color::Green))),
                Line::raw(""),
                Line::from(format!("输出文件：{}", output.display())),
                Line::raw(""),
                Line::from("Enter/Esc：返回首页  Q：退出"),
            ]
        },
    );
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" 结果 / 错误 ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.result_scroll(), 0)),
        area,
    );
}
