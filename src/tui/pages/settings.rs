use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::App;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let config = &app.config;
    let lines = vec![
        Line::from(format!(
            "翻译服务：{} / {}",
            config.translator.provider, config.translator.api_format
        )),
        Line::from(format!("翻译端点：{}", config.translator.base_url)),
        Line::from(format!("翻译模型：{}", config.translator.model)),
        Line::from(format!(
            "翻译 API 密钥：{}",
            config.translator.api_key.masked()
        )),
        Line::from(format!(
            "分块 / 前文 / 后文 / 重试：{} / {} / {} / {}",
            config.translator.chunk_size,
            config.translator.context_before,
            config.translator.context_after,
            config.translator.max_retries
        )),
        Line::raw(""),
        Line::from(format!("语音识别服务：{}", config.stt.provider)),
        Line::from(format!("语音识别端点：{}", config.stt.base_url)),
        Line::from(format!("语音识别模型：{}", config.stt.model)),
        Line::from(format!(
            "语音识别分片 / 重叠：{} 秒 / {} 秒",
            config.stt.chunk_seconds, config.stt.chunk_overlap_seconds
        )),
        Line::from(format!(
            "语音识别 API 密钥：{}",
            config.stt.api_key.masked()
        )),
        Line::from(format!("HTTP 超时：{} 秒", config.http_timeout.as_secs())),
        Line::from(format!("允许覆盖输出：{}", config.output_overwrite)),
        Line::raw(""),
        Line::from("R：重新加载 .env  Esc/H：返回首页  Q：退出"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" 设置 ").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}
