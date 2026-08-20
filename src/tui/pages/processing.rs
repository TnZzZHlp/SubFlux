use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Gauge, Paragraph},
};

use crate::app::{App, BatchFileProgress, BatchProcessingState};

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(batch) = &app.processing.batch {
        render_batch(frame, app, batch, area);
    } else {
        render_single(frame, app, area);
    }
}

fn render_single(frame: &mut Frame, app: &App, area: Rect) {
    let body = Layout::vertical([Constraint::Min(7), Constraint::Length(3)]).split(area);
    let total = app
        .processing
        .total
        .map_or_else(|| "?".into(), |total| total.to_string());
    let request = app
        .processing
        .request
        .map_or_else(|| "-".into(), |request| request.to_string());
    let lines = vec![
        Line::from(format!(
            "视频：{}",
            if app.video_path.is_empty() {
                "外部字幕"
            } else {
                &app.video_path
            }
        )),
        Line::from(format!("→ {}", app.processing.stage)),
        Line::from(format!(
            "当前文件进度：{} / {total}",
            app.processing.completed
        )),
        Line::from(format!("请求：{request}")),
        Line::from(format!("错误次数：{}", app.processing.errors)),
        Line::raw(""),
        Line::from("C 或 Esc：取消任务"),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().title(" 处理中 ").borders(Borders::ALL)),
        body[0],
    );
    render_gauge(
        frame,
        body[1],
        "进度",
        app.processing.completed,
        app.processing.total,
    );
}

fn render_batch(frame: &mut Frame, app: &App, batch: &BatchProcessingState, area: Rect) {
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
            "批量总计：{completed} / {}（成功：{}，跳过：{}，失败：{}）",
            batch.total, batch.succeeded, batch.skipped, batch.failed
        )),
        Line::from(format!(
            "{active_label}  ↑/↓、PageUp/PageDown：查看  C/Esc：取消"
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().title(" 批量处理中 ").borders(Borders::ALL)),
        body[0],
    );
    render_gauge(frame, body[1], "批量总进度", completed, Some(batch.total));

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
        "第 {index}/{batch_total} 个：{} - {} - 请求：{request}",
        file.video.display(),
        file.stage
    );
    render_gauge(frame, area, &title, file.completed, file.total);
}

fn render_gauge(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    completed: usize,
    total: Option<usize>,
) {
    let progress = total
        .filter(|total| *total > 0)
        .map_or(0.0, |total| {
            f64::from(gauge_number(completed)) / f64::from(gauge_number(total))
        })
        .clamp(0.0, 1.0);
    let total = total.map_or_else(|| "?".into(), |total| total.to_string());
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
