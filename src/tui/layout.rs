// The five-row frame (status, search, tabs, main, footer) and the hit
// testing. The preview always sits to the right of the table, and parts are
// dropped as the terminal narrows instead of being stacked.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Below this terminal width there is no room for a preview beside the table.
pub(super) const PREVIEW_MIN_WIDTH: u16 = 100;

/// The table shows its Directory column only when it is at least this wide.
pub(super) const DIRECTORY_MIN_TABLE_WIDTH: u16 = 84;

/// Share of the main area the preview takes, in percent, and its widest size.
const PREVIEW_PERCENT: u16 = 40;
const PREVIEW_MAX_WIDTH: u16 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AppLayout {
    pub(super) status: Rect,
    pub(super) search: Rect,
    pub(super) tabs: Rect,
    pub(super) results: Rect,
    /// `None` when the preview is switched off or the terminal is too narrow.
    pub(super) preview: Option<Rect>,
    pub(super) footer: Rect,
}

impl AppLayout {
    /// Whether the table has room for the Directory column. It is the first
    /// thing to go as the terminal narrows; the preview goes next.
    pub(super) fn show_directory(&self) -> bool {
        self.results.width >= DIRECTORY_MIN_TABLE_WIDTH
    }
}

pub(super) fn app(area: Rect, show_preview: bool) -> AppLayout {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let main = rows[3];
    let (results, preview) = if show_preview && area.width >= PREVIEW_MIN_WIDTH {
        let preview_w = (main.width * PREVIEW_PERCENT / 100).min(PREVIEW_MAX_WIDTH);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(preview_w)])
            .split(main);
        (cols[0], Some(cols[1]))
    } else {
        (main, None)
    };

    AppLayout {
        status: rows[0],
        search: rows[1],
        tabs: rows[2],
        results,
        preview,
        footer: rows[4],
    }
}

/// The two panes the mouse wheel can scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pane {
    Results,
    Preview,
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

pub(super) fn pane_at(layout: &AppLayout, column: u16, row: u16) -> Option<Pane> {
    if contains(layout.results, column, row) {
        Some(Pane::Results)
    } else if layout.preview.is_some_and(|p| contains(p, column, row)) {
        Some(Pane::Preview)
    } else {
        None
    }
}

/// The index of the session drawn at a screen position, if any. The table has
/// a border and a header row above its first session, and a border below.
pub(super) fn row_at(
    results: Rect,
    table_offset: usize,
    session_count: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    let first_row = results.y + 2;
    let last_row = results.bottom().checked_sub(1)?;
    if column <= results.x || column + 1 >= results.right() {
        return None;
    }
    if row < first_row || row >= last_row {
        return None;
    }
    let index = table_offset + (row - first_row) as usize;
    (index < session_count).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_targets_follow_the_panes() {
        let l = at(160, true);
        let preview = l.preview.unwrap();
        assert_eq!(pane_at(&l, 5, l.results.y + 3), Some(Pane::Results));
        assert_eq!(
            pane_at(&l, preview.x + 2, preview.y + 3),
            Some(Pane::Preview)
        );
        assert_eq!(pane_at(&l, 5, 0), None, "status line");
        assert_eq!(pane_at(&l, 5, 2), None, "search box");
        assert_eq!(pane_at(&l, 5, l.footer.y), None, "footer");

        let no_preview = at(160, false);
        assert_eq!(
            pane_at(&no_preview, 150, no_preview.results.y + 3),
            Some(Pane::Results)
        );
    }

    #[test]
    fn clicks_map_to_sessions_below_the_header() {
        let results = Rect::new(0, 5, 96, 20);
        // y 5 is the border, y 6 the header, y 7 the first session.
        assert_eq!(row_at(results, 0, 50, 10, 5), None);
        assert_eq!(row_at(results, 0, 50, 10, 6), None);
        assert_eq!(row_at(results, 0, 50, 10, 7), Some(0));
        assert_eq!(row_at(results, 0, 50, 10, 9), Some(2));
        assert_eq!(row_at(results, 0, 50, 10, 23), Some(16));
        assert_eq!(row_at(results, 0, 50, 10, 24), None, "bottom border");
        assert_eq!(row_at(results, 0, 50, 0, 7), None, "left border");
        assert_eq!(row_at(results, 0, 50, 95, 7), None, "right border");
        assert_eq!(row_at(results, 0, 50, 96, 7), None, "outside");
    }

    #[test]
    fn clicks_account_for_scrolling_and_short_lists() {
        let results = Rect::new(0, 5, 96, 20);
        assert_eq!(row_at(results, 30, 50, 10, 7), Some(30));
        assert_eq!(row_at(results, 30, 50, 10, 10), Some(33));
        assert_eq!(row_at(results, 0, 2, 10, 8), Some(1));
        assert_eq!(row_at(results, 0, 2, 10, 9), None, "below the last session");
        assert_eq!(row_at(results, 0, 0, 10, 7), None, "empty list");
        assert_eq!(row_at(Rect::new(0, 0, 0, 0), 0, 5, 0, 0), None);
    }

    fn at(width: u16, show_preview: bool) -> AppLayout {
        app(Rect::new(0, 0, width, 40), show_preview)
    }

    #[test]
    fn rows_stack_top_to_bottom() {
        let l = at(160, true);
        assert_eq!((l.status.y, l.status.height), (0, 1));
        assert_eq!((l.search.y, l.search.height), (1, 3));
        assert_eq!((l.tabs.y, l.tabs.height), (4, 1));
        assert_eq!(l.results.y, 5);
        assert_eq!((l.footer.y, l.footer.height), (39, 1));
        assert_eq!(l.results.bottom(), l.footer.y);
    }

    #[test]
    fn wide_terminal_has_directory_and_preview_on_the_right() {
        let l = at(160, true);
        let preview = l.preview.expect("preview shown");
        assert!(preview.x > l.results.x, "preview sits to the right");
        assert_eq!(preview.y, l.results.y);
        assert_eq!(l.results.width + preview.width, 160);
        assert!(l.show_directory());
    }

    #[test]
    fn narrowing_drops_the_directory_first_then_the_preview() {
        let roomy = at(160, true);
        assert!(roomy.show_directory() && roomy.preview.is_some());

        let medium = at(120, true);
        assert!(medium.preview.is_some(), "preview is still there");
        assert!(!medium.show_directory(), "directory went first");

        let narrow = at(80, true);
        assert!(narrow.preview.is_none(), "then the preview goes");
        assert!(!narrow.show_directory());
        assert_eq!(narrow.results.width, 80);
    }

    #[test]
    fn preview_off_gives_the_table_the_full_width() {
        let l = at(160, false);
        assert!(l.preview.is_none());
        assert_eq!(l.results.width, 160);
        assert!(l.show_directory());
    }

    #[test]
    fn very_wide_terminals_cap_the_preview() {
        let l = at(300, true);
        assert_eq!(l.preview.unwrap().width, PREVIEW_MAX_WIDTH);
        assert_eq!(l.results.width, 300 - PREVIEW_MAX_WIDTH);
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let l = app(Rect::new(0, 0, 10, 4), true);
        assert!(l.preview.is_none());
        let _ = app(Rect::new(0, 0, 0, 0), true);
    }
}
