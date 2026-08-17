use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Gauge, Paragraph},
};

use crate::app::App;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let body = Layout::vertical([Constraint::Min(7), Constraint::Length(3)]).split(area);
    let progress = app
        .processing
        .total
        .filter(|total| *total > 0)
        .map_or(0.0, |total| {
            f64::from(gauge_number(app.processing.completed)) / f64::from(gauge_number(total))
        })
        .clamp(0.0, 1.0);
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
        Line::raw(""),
        Line::from(format!("→ {}", app.processing.stage)),
        Line::from(format!("进度：{} / {}", app.processing.completed, total)),
        Line::from(format!("请求：{request}")),
        Line::from(format!("错误次数：{}", app.processing.errors)),
        Line::raw(""),
        Line::from("C 或 Esc：取消任务"),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().title(" 处理中 ").borders(Borders::ALL)),
        body[0],
    );
    frame.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("进度"))
            .ratio(progress)
            .label(format!("{:>3.0}%", progress * 100.0)),
        body[1],
    );
}

fn gauge_number(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
