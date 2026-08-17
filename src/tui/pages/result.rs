use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{app::App, event::BatchSummary};

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let lines = app
        .result
        .batch
        .as_ref()
        .map_or_else(|| render_single_result(app), render_batch_result);
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

fn render_single_result(app: &App) -> Vec<Line<'static>> {
    app.result.output.as_ref().map_or_else(
        || {
            let error = app.result.error.as_deref().unwrap_or("未知错误");
            let mut lines = vec![
                Line::from(Span::styled("处理未完成", Style::default().fg(Color::Red))),
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
                Line::from(Span::styled("处理完成", Style::default().fg(Color::Green))),
                Line::raw(""),
                Line::from(format!("输出文件：{}", output.display())),
                Line::raw(""),
                Line::from("Enter/Esc：返回首页  Q：退出"),
            ]
        },
    )
}

fn render_batch_result(summary: &BatchSummary) -> Vec<Line<'static>> {
    let has_failures = !summary.failed.is_empty();
    let heading = if has_failures {
        Span::styled(
            "批量处理已结束（有失败）",
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled("批量处理完成", Style::default().fg(Color::Green))
    };
    let mut lines = vec![
        Line::from(heading),
        Line::raw(""),
        Line::from(format!(
            "总计：{}  成功：{}  失败：{}",
            summary.total,
            summary.succeeded.len(),
            summary.failed.len()
        )),
    ];
    if has_failures {
        lines.push(Line::raw(""));
        lines.push(Line::from("失败详情："));
        for failure in &summary.failed {
            lines.push(Line::from(format!("• {}", failure.video.display())));
            lines.extend(
                failure
                    .error
                    .lines()
                    .map(|line| Line::from(format!("  {line}"))),
            );
        }
        lines.extend([
            Line::raw(""),
            Line::from("↑/↓、PageUp/PageDown：滚动详情  Enter/Esc：返回首页  Q：退出"),
        ]);
    } else {
        lines.extend([Line::raw(""), Line::from("Enter/Esc：返回首页  Q：退出")]);
    }
    lines
}
