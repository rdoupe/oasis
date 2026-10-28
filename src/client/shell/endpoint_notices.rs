use super::*;
use ratatui::widgets::{Clear, Widget};

pub(super) fn render_mobile_banner(
    buffer: &mut Buffer,
    area: Rect,
    notice: &ClientVisibleEndpointNotice,
    offset_for_warning: bool,
    palette: &Palette,
) -> Rect {
    super::notifications::render_mobile_notice_banner(
        buffer,
        area,
        &notice.title,
        Some(&notice.body),
        palette.red,
        offset_for_warning,
        palette,
    )
}

pub(super) fn render_lifecycle_banner(
    buffer: &mut Buffer,
    area: Rect,
    label: &str,
    status: ClientEndpointStatus,
    top_offset: u16,
    palette: &Palette,
) -> Rect {
    if area.is_empty() || status == ClientEndpointStatus::Online {
        return Rect::default();
    }
    let (symbol, state, color) = endpoint_status_presentation(status, palette);
    let text = format!("{symbol} {label} · {state}");
    let width = u16::try_from(unicode_width::UnicodeWidthStr::width(text.as_str()) + 2)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let y = area
        .y
        .saturating_add(top_offset)
        .min(area.bottom().saturating_sub(1));
    let rect = Rect::new(area.right().saturating_sub(width), y, width, 1);
    Clear.render(rect, buffer);
    buffer.set_style(rect, Style::default().bg(palette.surface0));
    super::render::put_text(
        buffer,
        rect.x.saturating_add(1),
        rect.y,
        rect.width.saturating_sub(2),
        &text,
        Style::default().fg(color).bg(palette.surface0),
    );
    rect
}

/// A full-width bar across the top of the pane area while the active machine has
/// stopped answering, so a frozen screen reads as the connection, not the app.
pub(super) fn render_unresponsive_bar(
    buffer: &mut Buffer,
    area: Rect,
    label: &str,
    palette: &Palette,
) -> Rect {
    if area.is_empty() {
        return Rect::default();
    }
    let rect = Rect::new(area.x, area.y, area.width, 1);
    Clear.render(rect, buffer);
    buffer.set_style(rect, Style::default().bg(palette.surface0));
    super::render::put_text(
        buffer,
        rect.x.saturating_add(1),
        rect.y,
        rect.width.saturating_sub(2),
        &unresponsive_text(label),
        Style::default()
            .fg(palette.yellow)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    rect
}

pub(super) fn unresponsive_text(label: &str) -> String {
    format!(
        "◌ {label} isn't responding · the connection has stalled · what you type is sent once it answers"
    )
}

pub(super) fn render_notice(
    buffer: &mut Buffer,
    area: Rect,
    notice: &ClientVisibleEndpointNotice,
    top_offset: u16,
    palette: &Palette,
) -> Rect {
    super::notifications::render_notification_card(
        buffer,
        area,
        &notice.title,
        &notice.body,
        crate::config::ToastHerdrPosition::TopRight,
        top_offset,
        match notice.key.kind {
            ClientEndpointNoticeKind::Unsupported | ClientEndpointNoticeKind::Rejected => {
                palette.red
            }
            ClientEndpointNoticeKind::Timeout | ClientEndpointNoticeKind::Unavailable => {
                palette.yellow
            }
        },
        palette,
    )
}
