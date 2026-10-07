//! One-line diagnostics occupy free chrome space only; they never cover controls and expire.
use super::*;
use ratatui::layout::Rect;

const NOTICE_TTL: Duration = Duration::from_secs(10);
const MIN_NOTICE_WIDTH: u16 = 8;

#[derive(Default)]
pub(super) struct NoticeClock {
    shown: Option<(String, Instant)>,
}

/// Starts the display window for new notice text and clears notices whose window ended.
/// Returns true when the visible notice changed.
pub(super) fn tick(frontend: &mut ClientFrontend, now: Instant) -> bool {
    let Some(text) = frontend.notice.as_ref() else {
        frontend.notice_clock.shown = None;
        return false;
    };
    match &frontend.notice_clock.shown {
        Some((shown, since)) if shown == text => {
            if now.duration_since(*since) < NOTICE_TTL {
                return false;
            }
            frontend.notice = None;
            frontend.notice_clock.shown = None;
            true
        }
        _ => {
            frontend.notice_clock.shown = Some((text.clone(), now));
            false
        }
    }
}

/// The free span of the pane action bar between COPY and the layout buttons. Without an action
/// bar the sidebar's last row is used; the notice is never drawn over a button or pane.
pub(super) fn area(view: &super::super::chrome::ChromeView) -> Option<Rect> {
    let bar = view.layout.pane_actions;
    if bar.width > 0 && bar.height > 0 {
        let start = if view.pane_actions.copy.width > 0 {
            view.pane_actions.copy.right().saturating_add(2)
        } else {
            bar.x.saturating_add(1)
        };
        let end = if view.pane_actions.cycle_layout.width > 0 {
            view.pane_actions.cycle_layout.x.saturating_sub(2)
        } else {
            bar.right()
        };
        return (end > start && end - start >= MIN_NOTICE_WIDTH)
            .then(|| Rect::new(start, bar.y, end - start, 1));
    }
    let sidebar = view.layout.sidebar;
    // The sidebar's last column is its separator.
    let width = sidebar.width.saturating_sub(2);
    (sidebar.height > 0 && width >= MIN_NOTICE_WIDTH)
        .then(|| Rect::new(sidebar.x + 1, sidebar.bottom() - 1, width, 1))
}

pub(super) fn render(
    frontend: &ClientFrontend,
    frame: &mut ratatui::Frame,
    view: &super::super::chrome::ChromeView,
) {
    let (Some(text), Some(area)) = (frontend.notice.as_deref(), area(view)) else {
        return;
    };
    let text = truncate(text.lines().next().unwrap_or_default(), area.width);
    // Only the text cells take the error colour; the rest of the row keeps its own styling.
    frame.render_widget(
        ratatui::widgets::Paragraph::new(ratatui::text::Span::styled(
            text,
            ratatui::style::Style::default().fg(frontend.chrome.settings.palette.red),
        )),
        area,
    );
}

fn truncate(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if unicode_width::UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_truncates_to_the_free_width_with_an_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("endpoint did not acknowledge", 10), "endpoint …");
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(truncate("接続が切れました", 9).as_str()),
            9
        );
    }

    #[test]
    fn notice_area_sits_between_copy_and_layout_buttons() {
        let bar = Rect::new(30, 44, 130, 1);
        let rects = crate::ui::pane_action_bar_rects(bar, 4);
        let area = area_for(bar, rects, Rect::new(0, 0, 27, 45)).expect("free span");
        assert!(area.x >= rects.copy.right());
        assert!(area.right() <= rects.cycle_layout.x);
        assert_eq!(area.y, 44);
    }

    #[test]
    fn notice_area_falls_back_to_the_sidebar_bottom_row_without_an_action_bar() {
        let area = area_for(Rect::default(), Default::default(), Rect::new(0, 0, 27, 45))
            .expect("sidebar row");
        assert_eq!(area, Rect::new(1, 44, 25, 1));
    }

    fn area_for(bar: Rect, rects: crate::ui::PaneActionBarRects, sidebar: Rect) -> Option<Rect> {
        let mut view = super::super::super::chrome::ChromeView::empty_for_test();
        view.layout.pane_actions = bar;
        view.layout.sidebar = sidebar;
        view.pane_actions = rects;
        area(&view)
    }
}
