use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Page};

use super::pages;

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 42 || area.height < 12 {
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new("终端窗口过小，请调整到至少 42×12。"), area);
        return;
    }
    let areas = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(area);
    match app.page {
        Page::Home => pages::home::render(frame, app, areas[0]),
        Page::Videos => pages::videos::render(frame, app, areas[0]),
        Page::Settings => pages::settings::render(frame, app, areas[0]),
        Page::Tracks => pages::tracks::render(frame, app, areas[0]),
        Page::Processing => pages::processing::render(frame, app, areas[0]),
        Page::Result => pages::result::render(frame, app, areas[0]),
    }
    let footer = truncate_footer(
        app.status_message
            .clone()
            .unwrap_or_else(|| "字幕翻译器".into()),
        areas[1].width.saturating_sub(2).into(),
    );
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::TOP)),
        areas[1],
    );
}

fn truncate_footer(footer: String, max_width: usize) -> String {
    if UnicodeWidthStr::width(footer.as_str()) <= max_width {
        return footer;
    }

    let ellipsis = '…';
    let ellipsis_width = ellipsis.width().unwrap_or_default();
    if max_width < ellipsis_width {
        return String::new();
    }
    let available_width = max_width - ellipsis_width;
    let mut truncated = String::new();
    let mut used_width = 0;
    for character in footer.chars() {
        let character_width = character.width().unwrap_or_default();
        if used_width + character_width > available_width {
            break;
        }
        truncated.push(character);
        used_width += character_width;
    }
    truncated.push(ellipsis);
    truncated
}

#[cfg(test)]
mod tests {
    use super::truncate_footer;

    #[test]
    fn footer_truncation_preserves_chinese_character_boundaries() {
        assert_eq!(truncate_footer("视频字幕翻译器".into(), 5), "视频…");
    }
}
