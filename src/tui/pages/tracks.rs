use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, ProbeStatus};

use super::{super::ui::LayoutMode, visible_range};
use crate::tui::ui::truncate_text;

pub(crate) fn render(frame: &mut Frame, app: &App, area: Rect, _mode: LayoutMode) {
    let width = usize::from(area.width.saturating_sub(4));
    let status = match &app.probe_status {
        ProbeStatus::Idle => "尚未探测".into(),
        ProbeStatus::Loading => "正在探测…".into(),
        ProbeStatus::Ready(count) => format!("已加载 {count} 条轨道"),
        ProbeStatus::Failed(error) => format!("探测失败：{error}"),
    };
    let video = if app.video_path.is_empty() {
        "<尚未设置视频>".into()
    } else {
        truncate_text(&app.video_path, width.saturating_sub(6))
    };
    let inner_height = usize::from(area.height.saturating_sub(2));
    let capacity = inner_height.saturating_sub(3);
    let range = visible_range(app.track_cursor, app.tracks.subtitle_tracks.len(), capacity);
    let mut lines = vec![
        Line::from(format!("视频：{video}")),
        Line::from(Span::styled(status, Style::default().fg(Color::DarkGray))),
        Line::raw(""),
    ];
    if app.tracks.subtitle_tracks.is_empty() {
        lines.push(Line::from("没有可显示的字幕轨，请按 P 探测。"));
    } else {
        for index in range {
            let track = &app.tracks.subtitle_tracks[index];
            let active = index == app.track_cursor;
            let style = if active {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if track.is_text() {
                Style::default()
            } else {
                Style::default().fg(Color::Yellow)
            };
            lines.push(Line::from(vec![
                Span::styled(if active { "› " } else { "  " }, style),
                Span::styled(
                    truncate_text(&track.display_label(), width.saturating_sub(2)),
                    style,
                ),
            ]));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" 字幕轨道选择 ")
                .borders(Borders::ALL),
        ),
        area,
    );
}
