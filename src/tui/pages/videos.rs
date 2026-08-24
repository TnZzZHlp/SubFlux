use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;

use super::{super::ui::LayoutMode, visible_range};
use crate::tui::ui::truncate_text;

pub(crate) fn render(frame: &mut Frame, app: &App, area: Rect, _mode: LayoutMode) {
    let inner_height = usize::from(area.height.saturating_sub(2));
    let capacity = inner_height.saturating_sub(2);
    let range = visible_range(app.video_cursor, app.video_candidates.len(), capacity);
    let range_label = if range.is_empty() {
        "无可用视频".into()
    } else {
        format!(
            "第 {}-{} 项，共 {} 项",
            range.start + 1,
            range.end,
            app.video_candidates.len()
        )
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "选择单个视频",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  {range_label}")),
        ]),
        Line::raw(""),
    ];
    let width = usize::from(area.width.saturating_sub(6));
    for index in range {
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
            Span::styled(
                truncate_text(&app.video_candidates[index].display().to_string(), width),
                style,
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" 视频选择 / 批量处理 ")
                .borders(Borders::ALL),
        ),
        area,
    );
}
