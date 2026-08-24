use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{app::App, event::BatchSummary};

use super::{super::ui::LayoutMode, visible_range};
use crate::tui::ui::truncate_text;

pub(crate) fn render(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    if let Some(summary) = &app.result.batch {
        render_batch_result(frame, app, summary, area, mode);
    } else {
        render_single_result(frame, app, area);
    }
}

fn render_single_result(frame: &mut Frame, app: &App, area: Rect) {
    let (title, color, mut lines) = app.result.output.as_ref().map_or_else(
        || {
            let error = app.result.error.as_deref().unwrap_or("未知错误");
            let mut lines = vec![Line::from(Span::styled(
                "任务未完成",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ))];
            lines.push(Line::raw(""));
            lines.extend(error.lines().map(|line| Line::from(line.to_owned())));
            (" 处理未完成 ", Color::Red, lines)
        },
        |output| {
            (
                " 处理完成 ",
                Color::Green,
                vec![
                    Line::from(Span::styled(
                        "字幕已成功写入",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::raw(""),
                    Line::from(format!("输出文件：{}", output.display())),
                ],
            )
        },
    );
    if lines.is_empty() {
        lines.push(Line::from("没有结果信息"));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(Span::styled(title, Style::default().fg(color)))
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.result_scroll(), 0)),
        area,
    );
}

fn render_batch_result(
    frame: &mut Frame,
    app: &App,
    summary: &BatchSummary,
    area: Rect,
    mode: LayoutMode,
) {
    if mode == LayoutMode::Compact {
        render_compact_batch(frame, app, summary, area);
        return;
    }
    let sections = Layout::vertical([Constraint::Length(4), Constraint::Min(5)]).split(area);
    let (heading, color) = if !summary.failed.is_empty() {
        ("批量处理结束，部分任务失败", Color::Yellow)
    } else if !summary.skipped.is_empty() {
        ("批量处理完成，部分文件已跳过", Color::Yellow)
    } else {
        ("批量处理全部完成", Color::Green)
    };
    let summary_lines = vec![
        Line::from(Span::styled(
            heading,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "总计 {}    成功 {}    跳过 {}    失败 {}",
            summary.total,
            summary.succeeded.len(),
            summary.skipped.len(),
            summary.failed.len()
        )),
    ];
    frame.render_widget(
        Paragraph::new(summary_lines).block(
            Block::default()
                .title(" 批量结果摘要 ")
                .borders(Borders::ALL),
        ),
        sections[0],
    );

    if summary.failed.is_empty() {
        render_skipped(frame, app, summary, sections[1]);
        return;
    }

    let columns = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(sections[1]);
    render_failure_list(frame, app, summary, columns[0]);
    render_failure_detail(frame, app, summary, columns[1]);
}

fn render_failure_list(frame: &mut Frame, app: &App, summary: &BatchSummary, area: Rect) {
    let capacity = usize::from(area.height.saturating_sub(2));
    let range = visible_range(app.result.failed_cursor, summary.failed.len(), capacity);
    let width = usize::from(area.width.saturating_sub(6));
    let lines = range
        .map(|index| {
            let active = index == app.result.failed_cursor;
            let style = if active {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(if active { "› " } else { "  " }, style),
                Span::styled(
                    truncate_text(&summary.failed[index].video.display().to_string(), width),
                    style,
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(format!(
                    " 失败文件 {}/{} ",
                    app.result.failed_cursor + 1,
                    summary.failed.len()
                ))
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_failure_detail(frame: &mut Frame, app: &App, summary: &BatchSummary, area: Rect) {
    let Some(failure) = summary.failed.get(app.result.failed_cursor) else {
        return;
    };
    let mut lines = vec![
        Line::from(Span::styled(
            failure.video.display().to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    lines.extend(
        failure
            .error
            .lines()
            .map(|line| Line::from(line.to_owned())),
    );
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(if app.result.batch_job.is_some() {
                        " 失败详情 / 可按 R 重试 "
                    } else {
                        " 失败详情 "
                    })
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.result_scroll(), 0)),
        area,
    );
}

fn render_skipped(frame: &mut Frame, app: &App, summary: &BatchSummary, area: Rect) {
    let lines = if summary.skipped.is_empty() {
        vec![Line::from("所有视频均已成功处理。")]
    } else {
        summary
            .skipped
            .iter()
            .map(|video| Line::from(format!("- {}", video.display())))
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" 跳过的文件 ").borders(Borders::ALL))
            .scroll((app.result_scroll(), 0)),
        area,
    );
}

fn render_compact_batch(frame: &mut Frame, app: &App, summary: &BatchSummary, area: Rect) {
    let mut lines = vec![Line::from(format!(
        "总计 {}  成功 {}  跳过 {}  失败 {}",
        summary.total,
        summary.succeeded.len(),
        summary.skipped.len(),
        summary.failed.len()
    ))];
    if let Some(failure) = summary.failed.get(app.result.failed_cursor) {
        lines.push(Line::from(format!(
            "失败 {}/{}：{}",
            app.result.failed_cursor + 1,
            summary.failed.len(),
            failure.video.display()
        )));
        lines.extend(
            failure
                .error
                .lines()
                .map(|line| Line::from(line.to_owned())),
        );
    } else if !summary.skipped.is_empty() {
        lines.push(Line::from(format!(
            "已跳过 {} 个文件",
            summary.skipped.len()
        )));
    } else {
        lines.push(Line::from("所有视频均已成功处理。"));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" 批量处理结果 ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.result_scroll(), 0)),
        area,
    );
}
