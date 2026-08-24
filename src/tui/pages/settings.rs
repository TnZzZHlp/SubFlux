use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, ReloadStatus};

use super::super::ui::LayoutMode;
use crate::tui::ui::truncate_text;

pub(crate) fn render(frame: &mut Frame, app: &App, area: Rect, mode: LayoutMode) {
    let lines = if mode == LayoutMode::Full {
        full_lines(app)
    } else {
        compact_lines(app, usize::from(area.width.saturating_sub(4)))
    };
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" 配置诊断 / 只读 ")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn full_lines(app: &App) -> Vec<Line<'static>> {
    let config = &app.config;
    vec![
        section("翻译服务"),
        Line::from(format!(
            "  接口：{} / {}",
            config.translator.provider, config.translator.api_format
        )),
        Line::from(format!("  端点：{}", config.translator.base_url)),
        Line::from(format!("  模型：{}", config.translator.model)),
        Line::from(format!(
            "  API 密钥：{}",
            config.translator.api_key.masked()
        )),
        Line::from(format!(
            "  分块 / 前文 / 后文 / 重试：{} / {} / {} / {}",
            config.translator.chunk_size,
            config.translator.context_before,
            config.translator.context_after,
            config.translator.max_retries
        )),
        section("语音识别"),
        Line::from(format!("  服务：{}", config.stt.provider)),
        Line::from(format!("  端点：{}", config.stt.base_url)),
        Line::from(format!("  模型：{}", config.stt.model)),
        Line::from(format!("  API 密钥：{}", config.stt.api_key.masked())),
        Line::from(format!(
            "  分片 / 重叠：{} 秒 / {} 秒",
            config.stt.chunk_seconds, config.stt.chunk_overlap_seconds
        )),
        section("运行参数"),
        Line::from(format!(
            "  HTTP 超时：{} 秒    批量并行数：{}",
            config.http_timeout.as_secs(),
            config.batch_concurrency
        )),
        reload_line(&app.reload_status),
    ]
}

fn compact_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let config = &app.config;
    vec![
        Line::from(truncate_text(
            &format!(
                "翻译：{} / {} / {}",
                config.translator.provider, config.translator.api_format, config.translator.model
            ),
            width,
        )),
        Line::from(format!("翻译密钥：{}", config.translator.api_key.masked())),
        Line::from(truncate_text(
            &format!("语音识别：{} / {}", config.stt.provider, config.stt.model),
            width,
        )),
        Line::from(format!("语音识别密钥：{}", config.stt.api_key.masked())),
        reload_line(&app.reload_status),
    ]
}

fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_owned(),
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    ))
}

fn reload_line(status: &ReloadStatus) -> Line<'static> {
    let (text, color) = match status {
        ReloadStatus::Idle => ("配置状态：已加载", Color::DarkGray),
        ReloadStatus::Loading => ("配置状态：正在重新加载…", Color::Yellow),
        ReloadStatus::Succeeded => ("配置状态：重新加载成功", Color::Green),
        ReloadStatus::Failed(error) => {
            return Line::from(Span::styled(
                format!("配置状态：重新加载失败：{error}"),
                Style::default().fg(Color::Red),
            ));
        }
    };
    Line::from(Span::styled(text, Style::default().fg(color)))
}
