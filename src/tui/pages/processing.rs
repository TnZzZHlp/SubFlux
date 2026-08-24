use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
};

use crate::app::{App, BatchFileProgress, BatchProcessingState};

use super::super::ui::LayoutMode;
use crate::tui::ui::truncate_text;

pub(crate) fn render(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    if let Some(batch) = &app.processing.batch {
        render_batch(frame, app, batch, area, mode);
    } else {
        render_single(frame, app, area, mode);
    }
}

fn render_single(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let subject = app
        .processing
        .subject
        .as_ref()
        .map_or_else(|| "正在准备输入".into(), |path| path.display().to_string());
    let progress = app.processing.total.map_or_else(
        || "等待进度数据".into(),
        |total| format!("{} / {total}", app.processing.completed),
    );
    let request = app.processing.request.map_or_else(
        || "尚未发送翻译请求".into(),
        |request| format!("请求 {request}"),
    );
    let width = usize::from(area.width.saturating_sub(6));
    let lines = vec![
        Line::from(Span::styled(
            app.processing.stage.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("任务：{}", truncate_text(&subject, width))),
        Line::from(format!("进度：{progress}    {request}")),
        Line::raw(""),
        Line::from(Span::styled(
            "C 或 Esc：取消当前任务",
            Style::default().fg(Color::Yellow),
        )),
    ];
    if mode == LayoutMode::Full
        && let Some(total) = app.processing.total.filter(|total| *total > 0)
    {
        let body = Layout::vertical([Constraint::Min(5), Constraint::Length(3)]).split(area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .title(" 单文件处理进度 ")
                    .borders(Borders::ALL),
            ),
            body[0],
        );
        render_gauge(frame, body[1], "总进度", app.processing.completed, total);
    } else {
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .title(" 单文件处理进度 ")
                    .borders(Borders::ALL),
            ),
            area,
        );
    }
}

fn render_batch(
    frame: &mut Frame,
    app: &App,
    batch: &BatchProcessingState,
    area: Rect,
    mode: LayoutMode,
) {
    if mode == LayoutMode::Compact {
        render_compact_batch(frame, app, batch, area);
        return;
    }
    let body = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(3),
        Constraint::Min(3),
    ])
    .split(area);
    let completed = batch.succeeded + batch.skipped + batch.failed;
    let active_count = batch.active.len();
    let visible_count = usize::from(body[2].height / 3).max(1);
    let start = usize::from(app.processing_scroll()).min(active_count.saturating_sub(1));
    let end = start.saturating_add(visible_count).min(active_count);
    let active_label = if active_count == 0 {
        "运行中：0 个".into()
    } else {
        format!("运行中：{active_count} 个（显示 {}-{}）", start + 1, end)
    };
    let lines = vec![
        Line::from(format!(
            "总计：{completed} / {}    成功 {}    跳过 {}    失败 {}",
            batch.total, batch.succeeded, batch.skipped, batch.failed
        )),
        Line::from(format!("{active_label}    C/Esc：取消")),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" 批量处理进度 ")
                .borders(Borders::ALL),
        ),
        body[0],
    );
    render_gauge(frame, body[1], "批量总进度", completed, batch.total.max(1));

    let rows = Layout::vertical(vec![Constraint::Length(3); visible_count]).split(body[2]);
    for ((index, file), row) in batch
        .active
        .iter()
        .skip(start)
        .take(visible_count)
        .zip(rows.iter().copied())
    {
        render_file_gauge(frame, row, *index, batch.total, file);
    }
}

fn render_compact_batch(frame: &mut Frame, app: &App, batch: &BatchProcessingState, area: Rect) {
    let completed = batch.succeeded + batch.skipped + batch.failed;
    let capacity = usize::from(area.height.saturating_sub(4));
    let start = usize::from(app.processing_scroll()).min(batch.active.len().saturating_sub(1));
    let width = usize::from(area.width.saturating_sub(8));
    let mut lines = vec![Line::from(format!(
        "{completed}/{}  成功 {}  跳过 {}  失败 {}  运行中 {}",
        batch.total,
        batch.succeeded,
        batch.skipped,
        batch.failed,
        batch.active.len()
    ))];
    for (index, file) in batch.active.iter().skip(start).take(capacity) {
        lines.push(Line::from(format!(
            "{index}/{}  {}  {}",
            batch.total,
            truncate_text(&file.video.display().to_string(), width),
            file.stage
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" 批量处理进度 / C 或 Esc 取消 ")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_file_gauge(
    frame: &mut Frame,
    area: Rect,
    index: usize,
    batch_total: usize,
    file: &BatchFileProgress,
) {
    let request = file
        .request
        .map_or_else(|| "-".into(), |request| request.to_string());
    let title = format!(
        "第 {index}/{batch_total} 个：{} - {} - 请求 {request}",
        file.video.display(),
        file.stage
    );
    if let Some(total) = file.total.filter(|total| *total > 0) {
        render_gauge(frame, area, &title, file.completed, total);
    } else {
        frame.render_widget(
            Paragraph::new("等待进度数据")
                .block(Block::default().borders(Borders::ALL).title(title)),
            area,
        );
    }
}

fn render_gauge(frame: &mut Frame, area: Rect, title: &str, completed: usize, total: usize) {
    let progress =
        (f64::from(gauge_number(completed)) / f64::from(gauge_number(total))).clamp(0.0, 1.0);
    frame.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title(title))
            .ratio(progress)
            .label(format!("{completed} / {total}  {:>3.0}%", progress * 100.0)),
        area,
    );
}

fn gauge_number(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
