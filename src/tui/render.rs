// The screen: a status line, a framed search box with colored filter
// keywords, agent tabs with counts, the session table with pin and fork
// marks, a preview to its right and a footer of key chips.

use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Padding, Paragraph, Row, Table, TableState, Wrap,
};

use super::app::{AppView, ForkChoice, Refresh};
use super::format;
use super::keys::{self, MenuAction};
use super::layout::{self, AppLayout};
use super::theme::{agent_color, THEME};
use crate::config::{AgentId, Rgb};
use crate::query::{self, Keyword};
use crate::search::{self, SearchResult};

fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Shown in the empty search box; it teaches the filter syntax.
pub(super) const SEARCH_HINT: &str = "Search titles, messages, paths. Try agent:claude";

const PIN_MARK: &str = "★";
const FORK_MARK: &str = "↳";
const SELECTED_MARK: &str = "▸ ";

const AGENT_W: u16 = 9;
const TURNS_W: u16 = 5;
const AGE_W: u16 = 5;
const DIR_MIN: u16 = 16;
const DIR_MAX: u16 = 36;

pub(super) fn draw(f: &mut Frame, app: &super::app::App) {
    let view = app.view();
    let area = f.area();
    let layout = layout::app(area, view.show_preview);

    draw_status(f, layout.status, &view);
    draw_search(f, layout.search, &view);
    draw_tabs(f, layout.tabs, &view);
    let table_offset = draw_results(f, &layout, &view);
    app.record_frame(layout, table_offset);
    if let Some(preview) = layout.preview {
        draw_preview(f, preview, &view);
    }
    draw_footer(f, layout.footer, &view);

    if let Some((input, cursor)) = view.rename_input {
        draw_rename_modal(f, area, input, cursor);
    }
    if let Some((agent, choices, selected)) = view.fork_modal {
        draw_fork_modal(f, area, agent, choices, selected);
    }
    if view.hide_modal {
        draw_hide_modal(f, area);
    }
    if let Some(selected) = view.actions_modal {
        draw_actions_modal(f, area, &view, selected);
    }
    if view.help_open {
        draw_help_modal(f, area);
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// True while any pop-up covers the main screen.
fn popup_is_open(view: &AppView) -> bool {
    view.rename_input.is_some()
        || view.fork_modal.is_some()
        || view.hide_modal
        || view.actions_modal.is_some()
        || view.help_open
}

// Every pop-up is drawn with the four helpers below, so they all share one
// frame, one selection bar and one hint line.

/// Cells a pop-up loses to its border and padding, left and right together.
const POPUP_CHROME: u16 = 4;

/// Clears a centered box with `rows` text lines and returns it.
fn open_popup(f: &mut Frame, area: Rect, width: u16, rows: usize) -> Rect {
    let popup = centered(area, width, rows as u16 + 2);
    f.render_widget(ratatui::widgets::Clear, popup);
    popup
}

/// The shared frame: a rounded border in the accent color with the title in
/// it. `danger` is for the one pop-up that confirms something destructive.
fn popup_block(title: &str, danger: bool) -> Block<'static> {
    let color = if danger { THEME.error } else { THEME.accent };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1))
}

/// The last line of a pop-up: "↑↓ move   Enter choose   Esc close".
fn hint_line(parts: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, action)) in parts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            (*key).to_string(),
            Style::default()
                .fg(THEME.secondary)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::default().fg(THEME.muted),
        ));
    }
    Line::from(spans)
}

/// Plain supporting text inside a pop-up.
fn note_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(THEME.secondary),
    ))
}

/// One row to choose from: "▸ label          right", `width` cells wide. The
/// selected row gets the same bar as the selected session in the list.
fn choice_line(
    label: &str,
    right: &str,
    selected: bool,
    enabled: bool,
    width: usize,
) -> Line<'static> {
    let label = format::truncate_end(label, width.saturating_sub(right.chars().count() + 3));
    let pad = width.saturating_sub(2 + label.chars().count() + right.chars().count());
    let bar = if selected {
        Style::default().bg(THEME.selected_bg)
    } else {
        Style::default()
    };
    let label_style = if !enabled {
        bar.fg(THEME.muted)
    } else if selected {
        bar.fg(THEME.selected_fg).add_modifier(Modifier::BOLD)
    } else {
        bar.fg(THEME.foreground)
    };
    let right_style = if selected && enabled {
        bar.fg(THEME.secondary)
    } else {
        bar.fg(THEME.muted)
    };
    Line::from(vec![
        Span::styled(
            if selected { SELECTED_MARK } else { "  " },
            bar.fg(THEME.accent),
        ),
        Span::styled(label, label_style),
        Span::styled(" ".repeat(pad), bar),
        Span::styled(right.to_string(), right_style),
    ])
}

const MOVE_CHOOSE_CLOSE: [(&str, &str); 3] =
    [("↑↓", "move"), ("Enter", "choose"), ("Esc", "close")];

fn inner_width(popup: Rect) -> usize {
    popup.width.saturating_sub(POPUP_CHROME) as usize
}

/// The Ctrl+O menu: each action on the selected session, with the key that
/// runs it directly.
fn draw_actions_modal(f: &mut Frame, area: Rect, view: &AppView, selected: usize) {
    let popup = open_popup(f, area, 44, MenuAction::ALL.len() + 2);
    let width = inner_width(popup);

    let mut lines = Vec::new();
    for (i, action) in MenuAction::ALL.iter().enumerate() {
        let label = match action {
            MenuAction::Pin if view.selected_is_pinned => "Unpin",
            other => other.label(),
        };
        let available = *action != MenuAction::Fork || view.can_fork_selected;
        lines.push(choice_line(
            label,
            action.shortcut(),
            i == selected,
            available,
            width,
        ));
    }
    lines.push(Line::from(""));
    lines.push(hint_line(&MOVE_CHOOSE_CLOSE));

    f.render_widget(
        Paragraph::new(lines).block(popup_block("Actions", false)),
        popup,
    );
}

/// The F1 overlay: every key on one screen. On a short terminal the blank
/// lines between the sections go, so the last rows are not cut off.
fn draw_help_modal(f: &mut Frame, area: Rect) {
    let sections = keys::KEY_HELP.len();
    let rows: usize = keys::KEY_HELP
        .iter()
        .map(|(_, r)| r.len() + 1)
        .sum::<usize>()
        + 1;
    let roomy = rows + sections + 4 <= area.height as usize;
    let popup = open_popup(f, area, 78, if roomy { rows + sections } else { rows });

    let mut lines = Vec::new();
    for (section, entries) in keys::KEY_HELP {
        lines.push(Line::from(Span::styled(
            (*section).to_string(),
            Style::default()
                .fg(THEME.accent)
                .add_modifier(Modifier::BOLD),
        )));
        for (key, action) in *entries {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {key:<22}"),
                    Style::default()
                        .fg(THEME.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled((*action).to_string(), Style::default().fg(THEME.secondary)),
            ]));
        }
        if roomy {
            lines.push(Line::from(""));
        }
    }
    // The closing hint always shows; rows that do not fit give way to it.
    let body = (popup.height.saturating_sub(3)) as usize;
    if lines.len() > body {
        lines.truncate(body);
        lines.push(hint_line(&[
            ("Any key", "close"),
            ("Taller window", "shows the rest"),
        ]));
    } else {
        lines.push(hint_line(&[("Any key", "close")]));
    }

    f.render_widget(
        Paragraph::new(lines).block(popup_block("Keys", false)),
        popup,
    );
}

fn draw_hide_modal(f: &mut Frame, area: Rect) {
    let popup = open_popup(f, area, 52, 4);
    let lines = vec![
        Line::from(Span::styled(
            "Hide this session from hopback?",
            Style::default().fg(THEME.foreground),
        )),
        note_line("The agent's own files are not touched."),
        Line::from(""),
        hint_line(&[("Enter", "hide"), ("Esc", "cancel")]),
    ];
    f.render_widget(
        Paragraph::new(lines).block(popup_block("Hide", true)),
        popup,
    );
}

fn draw_rename_modal(f: &mut Frame, area: Rect, input: &str, cursor: usize) {
    let popup = open_popup(f, area, 64, 4);
    // The two-cell prompt comes before the name.
    let text_width = inner_width(popup).saturating_sub(2);
    let (visible, cursor_col) = keys::input_view(input, cursor, text_width);

    let lines = vec![
        Line::from(vec![
            Span::styled(
                "› ",
                Style::default()
                    .fg(THEME.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(visible, Style::default().fg(THEME.foreground)),
        ]),
        Line::from(""),
        hint_line(&[("Enter", "save"), ("Esc", "cancel"), ("Ctrl+U", "clear")]),
        Line::from(Span::styled(
            "Save it empty to bring back the original title.",
            Style::default().fg(THEME.muted),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).block(popup_block("Rename", false)),
        popup,
    );
    if popup.width > POPUP_CHROME && popup.height > 2 {
        f.set_cursor_position((popup.x + 4 + cursor_col as u16, popup.y + 1));
    }
}

fn draw_fork_modal(
    f: &mut Frame,
    area: Rect,
    agent: &str,
    choices: &[ForkChoice],
    selected: usize,
) {
    let popup = open_popup(f, area, 44, choices.len() + 3);
    let width = inner_width(popup);

    let mut lines = vec![note_line(format!("Fork this {agent} session as"))];
    for (i, choice) in choices.iter().enumerate() {
        let label = match choice {
            ForkChoice::Native => format!("Native {agent} fork"),
            ForkChoice::Translated(target) => format!("Translated {} fork", target.badge()),
        };
        lines.push(choice_line(&label, "", i == selected, true, width));
    }
    lines.push(Line::from(""));
    lines.push(hint_line(&MOVE_CHOOSE_CLOSE));

    f.render_widget(
        Paragraph::new(lines).block(popup_block("Fork", false)),
        popup,
    );
}

fn line_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// "refreshed: 1,123 sessions, 0 changed, 14 ms", or what is happening now.
pub(super) fn refresh_text(refresh: Refresh, indexed_all: usize) -> String {
    let sessions = format::group_thousands(indexed_all);
    match refresh {
        Refresh::Running => format!("refreshing… {sessions} sessions"),
        Refresh::Done { changed, millis } => format!(
            "refreshed: {sessions} sessions, {} changed, {} ms",
            format::group_thousands(changed),
            format::group_thousands(millis as usize)
        ),
        Refresh::Skipped => format!("not refreshed: {sessions} sessions"),
    }
}

/// Top line: name and version and the refresh summary on the left, how much
/// is shown and how long the search took on the right.
fn status_line(view: &AppView, width: u16) -> Line<'static> {
    let width = width as usize;
    let mut left = vec![
        Span::styled(
            "hopback",
            Style::default()
                .fg(THEME.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" v{}", env!("CARGO_PKG_VERSION"))),
    ];
    let right = format!(
        "{} shown / {} indexed   {:.1}ms ",
        format::group_thousands(view.shown),
        format::group_thousands(view.indexed),
        view.query_ms
    );
    let right_w = right.chars().count();

    let room = width.saturating_sub(line_width(&left) + right_w + 4);
    if room >= 12 {
        let style = if view.refresh == Refresh::Running {
            Style::default().fg(THEME.warning)
        } else {
            Style::default().fg(THEME.muted)
        };
        left.push(Span::raw("  "));
        left.push(Span::styled(
            format::truncate_end(&refresh_text(view.refresh, view.indexed_all), room),
            style,
        ));
    }

    let pad = width.saturating_sub(line_width(&left) + right_w);
    left.push(Span::raw(" ".repeat(pad)));
    left.push(Span::styled(right, Style::default().fg(THEME.muted)));
    Line::from(left)
}

fn draw_status(f: &mut Frame, area: Rect, view: &AppView) {
    f.render_widget(Paragraph::new(status_line(view, area.width)), area);
}

fn panel(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(THEME.panel_border))
        .title(Span::styled(title, Style::default().fg(THEME.secondary)))
}

/// One style per character of the search text. Filter keywords stand out:
/// `agent:` and friends in the info color, an agent's name in that agent's
/// color, other values in green, and a value hopback does not know in red
/// and struck through.
fn query_styles(text: &str) -> Vec<Style> {
    let plain = Style::default().fg(THEME.foreground);
    let wrong = Style::default().fg(THEME.error);
    let spans = query::keyword_spans(text);

    let mut styles = Vec::with_capacity(text.len());
    for (byte, ch) in text.char_indices() {
        let span = spans
            .iter()
            .find(|span| span.start <= byte && byte < span.end && span.value_start < span.end);
        let Some(span) = span else {
            styles.push(plain);
            continue;
        };
        let value = &text[span.value_start..span.end];
        let valid = query::is_valid_value(span.keyword, value);
        let style = if byte < span.value_start {
            if span.negated && byte == span.start {
                wrong.add_modifier(Modifier::BOLD)
            } else if valid {
                Style::default().fg(THEME.info).add_modifier(Modifier::BOLD)
            } else {
                wrong.add_modifier(Modifier::BOLD)
            }
        } else if !valid {
            wrong.add_modifier(Modifier::CROSSED_OUT)
        } else if ch == '!' {
            wrong.add_modifier(Modifier::BOLD)
        } else if ch == ',' {
            Style::default().fg(THEME.muted)
        } else {
            Style::default().fg(value_color(span.keyword, value, byte - span.value_start))
        };
        styles.push(style);
    }
    styles
}

/// Color of the value character at byte `offset` of a valid filter value: an
/// agent's own color inside its name, green for anything else.
fn value_color(keyword: Keyword, value: &str, offset: usize) -> Color {
    if keyword != Keyword::Agent {
        return THEME.success;
    }
    let mut start = 0;
    for part in value.split(',') {
        let end = start + part.len();
        if offset < end {
            let name = part.trim().trim_start_matches('!');
            return AgentId::resolve(name).map_or(THEME.success, agent_color);
        }
        start = end + 1;
    }
    THEME.success
}

/// The search text from character `start`, `len` characters long, as spans
/// colored by `query_styles`.
fn query_spans(text: &str, start: usize, len: usize) -> Vec<Span<'static>> {
    let styles = query_styles(text);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut last: Option<Style> = None;
    for (ch, style) in text.chars().zip(styles).skip(start).take(len) {
        match spans.last_mut() {
            Some(span) if last == Some(style) => span.content.to_mut().push(ch),
            _ => {
                spans.push(Span::styled(ch.to_string(), style));
                last = Some(style);
            }
        }
    }
    spans
}

fn draw_search(f: &mut Frame, area: Rect, view: &AppView) {
    let prompt = Span::styled(
        "› ",
        Style::default()
            .fg(THEME.accent)
            .add_modifier(Modifier::BOLD),
    );
    // Border and padding on each side, then the two-cell prompt.
    let text_width = area.width.saturating_sub(6) as usize;
    let (visible, cursor_col) = keys::input_view(view.query, view.cursor, text_width);
    let mut spans = vec![prompt];
    if view.query.is_empty() {
        spans.push(Span::styled(
            SEARCH_HINT,
            Style::default()
                .fg(THEME.muted)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        let shown = visible.chars().count();
        let start = view.cursor.saturating_sub(cursor_col);
        spans.extend(query_spans(view.query, start, shown));
        // The suggested rest of a value, dimmed, from the cursor on.
        if let Some(rest) = view.completion.as_deref() {
            let room = text_width.saturating_sub(shown);
            if room > 0 && cursor_col == shown {
                spans.push(Span::styled(
                    rest.chars().take(room).collect::<String>(),
                    Style::default().fg(THEME.muted),
                ));
            }
        }
    }
    let block = panel(" Search ").padding(Padding::horizontal(1));
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);

    if !popup_is_open(view) {
        let cx = area.x + 4 + cursor_col as u16;
        f.set_cursor_position((cx.min(area.x + area.width.saturating_sub(3)), area.y + 1));
    }
}

/// "All 1,123 · agy 296 · claude 168": one tab per agent that has sessions,
/// each in its own color, the active one filled in.
fn tabs_line(view: &AppView) -> Line<'static> {
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    spans.extend(tab(
        "All",
        view.indexed_all,
        view.all_tab_active,
        THEME.secondary,
    ));
    for (agent, count) in &view.agent_counts {
        spans.push(Span::styled(" · ", Style::default().fg(THEME.muted)));
        spans.extend(tab(
            agent.badge(),
            *count,
            view.active_agents.contains(agent),
            agent_color(*agent),
        ));
    }
    Line::from(spans)
}

fn tab(label: &str, count: usize, active: bool, color: Color) -> Vec<Span<'static>> {
    let count = format::group_thousands(count);
    if active {
        vec![Span::styled(
            format!(" {label} {count} "),
            Style::default()
                .fg(Color::Black)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        )]
    } else {
        vec![
            Span::styled(
                label.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {count}"), Style::default().fg(THEME.muted)),
        ]
    }
}

fn draw_tabs(f: &mut Frame, area: Rect, view: &AppView) {
    f.render_widget(Paragraph::new(tabs_line(view)), area);
}

/// Width of the Directory column for a table this wide; 0 hides it.
fn dir_width(layout: &AppLayout) -> u16 {
    if !layout.show_directory() {
        return 0;
    }
    (layout.results.width / 4).clamp(DIR_MIN, DIR_MAX)
}

/// Draws the table and returns its scroll offset (the first visible row).
fn draw_results(f: &mut Frame, layout: &AppLayout, view: &AppView) -> usize {
    let area = layout.results;
    let now = format::now_unix_secs();
    let home = std::env::var("HOME").unwrap_or_default();
    let dir_w = dir_width(layout);
    let title_w = title_width(area.width, dir_w);

    let rows: Vec<Row> = if view.results.is_empty() {
        let message = if view.indexing && view.indexed_all == 0 {
            "Reading sessions…"
        } else {
            "No sessions found"
        };
        vec![Row::new(vec![
            Cell::from(""),
            Cell::from(Span::styled(
                message,
                Style::default()
                    .fg(THEME.muted)
                    .add_modifier(Modifier::ITALIC),
            )),
        ])]
    } else {
        view.results
            .iter()
            .map(|r| {
                let key = r.session.key();
                result_row(
                    r,
                    view.query,
                    now,
                    &home,
                    (dir_w, title_w),
                    view.pinned.contains(&key),
                    view.forked.contains(&key),
                )
            })
            .collect()
    };

    let mut widths = vec![Constraint::Length(AGENT_W), Constraint::Min(16)];
    let mut header = vec![Cell::from("Agent"), Cell::from("Title")];
    if dir_w > 0 {
        widths.push(Constraint::Length(dir_w));
        header.push(Cell::from("Directory"));
    }
    widths.push(Constraint::Length(TURNS_W));
    header.push(Cell::from(Line::from("Turns").alignment(Alignment::Right)));
    widths.push(Constraint::Length(AGE_W));
    header.push(Cell::from(Line::from("Age").alignment(Alignment::Right)));

    let table = Table::new(rows, widths)
        .header(
            Row::new(header).style(
                Style::default()
                    .fg(THEME.secondary)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(panel(" Sessions "))
        .column_spacing(1)
        .row_highlight_style(
            Style::default()
                .bg(THEME.selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(Span::styled(
            SELECTED_MARK,
            Style::default().fg(THEME.accent),
        ));

    let mut state = TableState::default().with_offset(view.table_offset);
    if !view.results.is_empty() {
        state.select(Some(view.selected));
    }
    f.render_stateful_widget(table, area, &mut state);
    state.offset()
}

/// Cells left for the Title column once the borders, the selection marker,
/// the other columns and the gaps between columns are taken out.
fn title_width(table_width: u16, dir_w: u16) -> usize {
    let columns = if dir_w > 0 { 5 } else { 4 };
    let fixed = 2 + SELECTED_MARK.chars().count() as u16 + AGENT_W + dir_w + TURNS_W + AGE_W;
    table_width.saturating_sub(fixed + (columns - 1)) as usize
}

fn result_row<'a>(
    r: &'a SearchResult,
    query: &str,
    now: i64,
    home: &str,
    (dir_w, title_w): (u16, usize),
    pinned: bool,
    forked: bool,
) -> Row<'a> {
    let s = &r.session;
    let badge_cell = Cell::from(Span::styled(
        s.agent.badge().to_string(),
        Style::default()
            .fg(agent_color(s.agent))
            .add_modifier(Modifier::BOLD),
    ));

    let mut title = Vec::new();
    if pinned {
        title.push(Span::styled(
            format!("{PIN_MARK} "),
            Style::default().fg(THEME.pin),
        ));
    }
    if forked {
        title.push(Span::styled(
            format!("{FORK_MARK} "),
            Style::default().fg(THEME.fork),
        ));
    }
    // Cut long titles with an ellipsis instead of letting the cell clip them.
    let marks = title.len() * 2;
    let shown = format::truncate_end(&s.title, title_w.saturating_sub(marks).max(1));
    title.extend(
        highlight(&shown, query)
            .spans
            .into_iter()
            .map(|span| Span::styled(span.content.into_owned(), span.style)),
    );
    let title_cell = Cell::from(Line::from(title));

    let turns = if s.message_count > 0 {
        s.message_count.to_string()
    } else {
        "-".to_string()
    };
    let turns_cell = Cell::from(
        Line::from(Span::styled(turns, Style::default().fg(THEME.secondary)))
            .alignment(Alignment::Right),
    );

    let age_hours = ((now - s.timestamp).max(0) as f64) / 3600.0;
    let age_cell = Cell::from(
        Line::from(Span::styled(
            format::age_short(now, s.timestamp),
            Style::default().fg(rgb(format::age_color(age_hours))),
        ))
        .alignment(Alignment::Right),
    );

    let mut cells = vec![badge_cell, title_cell];
    if dir_w > 0 {
        let dir = format::truncate_left(
            &format::format_directory(&s.directory, home),
            dir_w as usize,
        );
        cells.push(Cell::from(Span::styled(
            dir,
            Style::default().fg(THEME.muted),
        )));
    }
    cells.push(turns_cell);
    cells.push(age_cell);
    Row::new(cells)
}

fn lower_with_map(text: &str) -> (String, Vec<(usize, usize)>) {
    let mut lc = String::with_capacity(text.len());
    let mut map: Vec<(usize, usize)> = Vec::with_capacity(text.len());
    let mut tmp = [0u8; 4];
    for (orig_off, ch) in text.char_indices() {
        let orig_end = orig_off + ch.len_utf8();
        for lo in ch.to_lowercase() {
            let s = lo.encode_utf8(&mut tmp);
            for _ in 0..s.len() {
                map.push((orig_off, orig_end));
            }
            lc.push_str(s);
        }
    }
    (lc, map)
}

fn highlight<'a>(text: &'a str, query: &str) -> Line<'a> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return Line::from(text);
    }

    let (lc, map) = lower_with_map(text);
    let mut spans: Vec<Span> = Vec::new();
    let hl = Style::default()
        .fg(THEME.match_fg)
        .bg(THEME.match_bg)
        .add_modifier(Modifier::BOLD);

    let mut i = 0;
    let mut last = 0;
    while i < lc.len() {
        let next = terms
            .iter()
            .filter_map(|t| lc[i..].find(t.as_str()).map(|rel| (i + rel, t.len())))
            .min_by_key(|(pos, _)| *pos);
        match next {
            Some((pos, len)) => {
                let orig_start = map[pos].0;
                let last_byte = (pos + len).saturating_sub(1).min(map.len() - 1);
                let orig_end = map[last_byte].1.max(orig_start);
                if orig_start > last {
                    spans.push(Span::raw(&text[last..orig_start]));
                }
                spans.push(Span::styled(text[orig_start..orig_end].to_string(), hl));
                last = orig_end;

                i = resume_lc_index(&map, orig_end).max(i + 1);
            }
            None => break,
        }
    }
    if last < text.len() {
        spans.push(Span::raw(&text[last..]));
    }
    Line::from(spans)
}

fn resume_lc_index(map: &[(usize, usize)], orig: usize) -> usize {
    map.iter()
        .position(|&(s, _)| s >= orig)
        .unwrap_or(map.len())
}

/// The part of a conversation to show: from the top, or from just before the
/// first match when there is a query.
fn preview_snippet<'a>(content: &'a str, query: &str) -> (&'a str, bool) {
    let start = search::first_match_offset(content, query)
        .map(|off| off.saturating_sub(200))
        .unwrap_or(0);
    let mut start = floor_char_boundary(content, start);
    // Start at the beginning of a message so its marker is not cut off.
    if start > 0 {
        if let Some(pos) = content[..start].rfind("\n\n") {
            start = pos + 2;
        }
    }
    let rest = &content[start..];
    let end = rest
        .char_indices()
        .nth(6_000)
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    (&rest[..end], start > 0)
}

/// Agent and title, then folder and date, then the conversation with the
/// user's prompts marked.
fn preview_lines<'a>(r: &'a SearchResult, query: &str, home: &str) -> Vec<Line<'a>> {
    let s = &r.session;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                s.agent.badge().to_string(),
                Style::default()
                    .fg(agent_color(s.agent))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                s.title.clone(),
                Style::default()
                    .fg(THEME.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format::format_directory(&s.directory, home),
                Style::default().fg(THEME.muted),
            ),
            Span::raw("  "),
            Span::styled(
                format::local_date_time(s.timestamp),
                Style::default().fg(THEME.muted),
            ),
        ]),
        Line::from(""),
    ];

    let (snippet, cut) = preview_snippet(&s.content, query);
    if cut {
        lines.push(Line::from(Span::styled(
            "…",
            Style::default().fg(THEME.muted),
        )));
    }
    let mut role = Role::Other;
    for raw in snippet.lines() {
        lines.push(preview_line(raw, query, s.agent, &mut role));
    }
    lines
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
    Other,
}

/// One line of the conversation. The first line of a message carries its
/// marker; `role` remembers whose message the following lines belong to.
fn preview_line<'a>(raw: &'a str, query: &str, agent: AgentId, role: &mut Role) -> Line<'a> {
    use crate::model::{ASSISTANT_PREFIX, USER_PREFIX};

    let user_style = Style::default().fg(THEME.user_text);
    let assistant_style = Style::default().fg(THEME.assistant_text);

    if raw.trim().is_empty() {
        return Line::from("");
    }
    if let Some(rest) = raw.strip_prefix(USER_PREFIX) {
        *role = Role::User;
        let mut spans = vec![Span::styled(
            "» ",
            Style::default()
                .fg(THEME.user_accent)
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(styled(
            highlight(rest, query),
            user_style.add_modifier(Modifier::BOLD),
        ));
        Line::from(spans)
    } else if let Some(rest) = raw.strip_prefix(ASSISTANT_PREFIX) {
        // A deeper indent is a continuation line of the current message
        // (code, a nested list), not the start of an assistant reply.
        if *role == Role::User && rest.starts_with(' ') {
            return Line::from(styled(highlight(raw, query), user_style));
        }
        *role = Role::Assistant;
        let mut spans = vec![Span::styled("● ", Style::default().fg(agent_color(agent)))];
        spans.extend(styled(highlight(rest, query), assistant_style));
        Line::from(spans)
    } else {
        let style = match *role {
            Role::User => user_style,
            Role::Assistant => assistant_style,
            Role::Other => Style::default().fg(THEME.secondary),
        };
        let mut spans = vec![Span::raw("  ")];
        spans.extend(styled(highlight(raw, query), style));
        Line::from(spans)
    }
}

/// Applies `base` to the spans of a highlighted line, leaving the match
/// styling on the matched spans.
fn styled<'a>(line: Line<'a>, base: Style) -> Vec<Span<'a>> {
    line.spans
        .into_iter()
        .map(|span| {
            if span.style.bg.is_some() {
                span
            } else {
                let style = base.patch(span.style);
                span.style(style)
            }
        })
        .collect()
}

fn draw_preview(f: &mut Frame, area: Rect, view: &AppView) {
    let block = panel(" Preview ").padding(Padding::horizontal(1));
    let Some(r) = view.results.get(view.selected) else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No session selected",
                Style::default().fg(THEME.muted),
            ))
            .block(block),
            area,
        );
        return;
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let para = Paragraph::new(preview_lines(r, view.query, &home))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((view.preview_scroll, 0));
    f.render_widget(para, area);
}

/// Each key in a highlighted chip, followed by what it does. The first chip
/// (Enter) is in the accent color.
pub(super) fn footer_line(keys: &[(&str, &str)], toast: Option<&str>, width: u16) -> Line<'static> {
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, (key, action)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        // The first chip is the main action and takes the accent color.
        let chip = if i == 0 {
            Style::default().fg(THEME.key_primary_fg).bg(THEME.accent)
        } else {
            Style::default().fg(THEME.key_fg).bg(THEME.key_bg)
        };
        spans.push(Span::styled(
            format!(" {key} "),
            chip.add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::default().fg(THEME.secondary),
        ));
    }
    if let Some(toast) = toast {
        let used = line_width(&spans) + toast.chars().count() + 1;
        if used <= width as usize {
            spans.push(Span::raw(" ".repeat(width as usize - used)));
            spans.push(Span::styled(
                toast.to_string(),
                Style::default().fg(THEME.success),
            ));
        }
    }
    Line::from(spans)
}

fn draw_footer(f: &mut Frame, area: Rect, view: &AppView) {
    let keys = keys::footer_keys(
        view.query.is_empty(),
        view.can_fork_selected,
        view.selected_is_pinned,
    );
    // A message on the right needs its own room, so chips give way to it.
    let reserved = view.toast.map_or(0, |toast| toast.chars().count() + 3);
    let keys = keys::fit_footer(keys, (area.width as usize).saturating_sub(reserved));
    f.render_widget(
        Paragraph::new(footer_line(&keys, view.toast, area.width)),
        area,
    );
}

fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Session;
    use crate::search::Filters;
    use crate::tui::app::App;
    use ratatui::backend::TestBackend;

    fn sess(id: &str, agent: AgentId, title: &str, dir: &str, age_secs: i64) -> Session {
        Session {
            id: id.into(),
            agent,
            title: title.into(),
            directory: dir.into(),
            timestamp: format::now_unix_secs() - age_secs,
            content: format!("» {title}\n\n  Working on it.\n\n» and add tests\n\n  Tests added."),
            message_count: 4,
            mtime: 0,
            yolo: false,
            source_path: None,
        }
    }

    fn fixture() -> App {
        let home = std::env::var("HOME").unwrap_or_default();
        let mut app = App::for_test(
            vec![
                sess(
                    "a",
                    AgentId::Claude,
                    "fix the auth middleware",
                    &format!("{home}/code/api"),
                    120,
                ),
                sess(
                    "b",
                    AgentId::Codex,
                    "refactor the uploader",
                    "/srv/app/very/deep/nested/project/folder/name",
                    7_200,
                ),
                sess(
                    "c",
                    AgentId::Antigravity,
                    "plan the migration",
                    "/work/db",
                    3 * 86_400,
                ),
                sess(
                    "d",
                    AgentId::Claude,
                    "write release notes",
                    "/work/docs",
                    40 * 86_400,
                ),
            ],
            "",
            Filters::default(),
        );
        app.finish_refresh_for_test(2, 14);
        app
    }

    fn buffer(app: &App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Renders the app at the given size and returns the screen as text rows.
    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        let buffer = buffer(app, width, height);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    /// The table's part of the screen: each row cut where the preview starts.
    fn table(app: &App, width: u16, height: u16) -> Vec<String> {
        let table_w = layout::app(Rect::new(0, 0, width, height), app.view().show_preview)
            .results
            .width as usize;
        screen(app, width, height)
            .into_iter()
            .map(|row| row.chars().take(table_w).collect())
            .collect()
    }

    fn find<'a>(rows: &'a [String], needle: &str) -> &'a String {
        rows.iter()
            .find(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", rows.join("\n")))
    }

    /// Column (in cells) where `needle` starts in `row`.
    fn column(row: &str, needle: &str) -> usize {
        let byte = row
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in {row:?}"));
        row[..byte].chars().count()
    }

    #[test]
    fn status_line_shows_version_refresh_summary_and_counts() {
        let rows = screen(&fixture(), 160, 30);
        let top = &rows[0];
        assert!(top.starts_with(&format!("hopback v{}", env!("CARGO_PKG_VERSION"))));
        assert!(
            top.contains("refreshed: 4 sessions, 2 changed, 14 ms"),
            "{top}"
        );
        assert!(top.contains("4 shown / 4 indexed"), "{top}");
        assert!(
            top.trim_end().ends_with("ms"),
            "search time on the right: {top}"
        );
        assert!(column(top, "4 shown") > column(top, "refreshed"));
    }

    #[test]
    fn refresh_text_covers_running_done_and_skipped() {
        assert_eq!(
            refresh_text(
                Refresh::Done {
                    changed: 0,
                    millis: 14
                },
                1_123
            ),
            "refreshed: 1,123 sessions, 0 changed, 14 ms"
        );
        assert_eq!(
            refresh_text(Refresh::Running, 12),
            "refreshing… 12 sessions"
        );
        assert_eq!(
            refresh_text(Refresh::Skipped, 12),
            "not refreshed: 12 sessions"
        );
    }

    #[test]
    fn search_box_is_framed_and_shows_the_hint_when_empty() {
        let rows = screen(&fixture(), 160, 30);
        assert!(rows[1].starts_with("╭ Search "), "{}", rows[1]);
        assert!(rows[2].contains(SEARCH_HINT), "{}", rows[2]);
        assert!(rows[3].starts_with('╰'), "{}", rows[3]);

        let mut app = fixture();
        app.set_query_for_test("agent:claude auth");
        let rows = screen(&app, 160, 30);
        assert!(rows[2].contains("› agent:claude auth"), "{}", rows[2]);
        assert!(!rows[2].contains("Try agent:claude"));
    }

    #[test]
    fn tabs_list_agents_with_counts() {
        let rows = screen(&fixture(), 160, 30);
        let tabs = &rows[4];
        assert!(tabs.contains("All 4"), "{tabs}");
        assert!(tabs.contains("agy 1"), "{tabs}");
        assert!(tabs.contains("claude 2"), "{tabs}");
        assert!(tabs.contains("codex 1"), "{tabs}");
        let order: Vec<usize> = ["All", "agy", "claude", "codex"]
            .iter()
            .map(|label| column(tabs, label))
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "tab order: {tabs}");
    }

    #[test]
    fn tabs_use_each_agents_color_and_fill_the_active_one() {
        let app = fixture();
        let line = tabs_line(&app.view());
        let claude = line
            .spans
            .iter()
            .find(|s| s.content == "claude")
            .expect("claude tab");
        assert_eq!(claude.style.fg, Some(agent_color(AgentId::Claude)));
        let codex = line.spans.iter().find(|s| s.content == "codex").unwrap();
        assert_eq!(codex.style.fg, Some(agent_color(AgentId::Codex)));
        assert_ne!(claude.style.fg, codex.style.fg);
        let all = line
            .spans
            .iter()
            .find(|s| s.content.contains("All"))
            .unwrap();
        assert!(all.style.bg.is_some(), "the active tab is filled");
    }

    #[test]
    fn table_has_the_five_columns_and_marks_the_selected_row() {
        let rows = table(&fixture(), 160, 30);
        let header = find(&rows, "Agent");
        for name in ["Agent", "Title", "Directory", "Turns", "Age"] {
            assert!(header.contains(name), "{name} missing in {header}");
        }
        let first = find(&rows, "fix the auth middleware");
        assert!(first.contains("▸ claude"), "selected row marker: {first}");
        assert!(first.contains("~/code/api"), "home is shortened: {first}");
        assert!(first.contains(" 2m"), "age: {first}");
        let second = find(&rows, "refactor the uploader");
        assert!(!second.contains('▸'));
        assert!(
            second.contains('…'),
            "long folders are cut with an ellipsis: {second}"
        );
        assert!(
            second.contains("folder/name"),
            "the end of the path is kept: {second}"
        );
        assert!(second.contains(" 2h"));
        assert!(find(&rows, "plan the migration").contains(" 3d"));
        assert!(find(&rows, "write release notes").contains(" 5w"));
    }

    #[test]
    fn long_titles_are_cut_with_an_ellipsis() {
        let long =
            "refactor the checkout flow so that discounts are only ever applied in one place";
        let mut app = App::for_test(
            vec![sess("a", AgentId::Claude, long, "/w", 60)],
            "",
            Filters::default(),
        );
        app.finish_refresh_for_test(0, 1);
        let rows = table(&app, 100, 12);
        let row = find(&rows, "refactor the checkout");
        assert!(row.contains('…'), "{row}");
        assert!(!row.contains("one place"));
        // The cut title leaves the Turns and Age columns intact.
        assert!(row.trim_end().ends_with("1m│"), "{row}");
        assert!(row.contains("    4 "), "{row}");

        // A wide table shows it whole.
        app.set_preview_for_test(false);
        let rows = table(&app, 200, 12);
        assert!(find(&rows, "refactor the checkout").contains("one place"));
    }

    #[test]
    fn pinned_and_forked_sessions_carry_marks() {
        let mut app = fixture();
        app.mark_for_test("b", AgentId::Codex, true, true);
        app.mark_for_test("c", AgentId::Antigravity, false, true);
        let rows = table(&app, 160, 30);
        let pinned = find(&rows, "refactor the uploader");
        assert!(pinned.contains("★ ↳ refactor"), "{pinned}");
        let forked = find(&rows, "plan the migration");
        assert!(
            forked.contains("↳ plan") && !forked.contains('★'),
            "{forked}"
        );
        assert!(!find(&rows, "write release notes").contains('↳'));

        // Pinned sessions float to the top of the table.
        let pinned_y = rows
            .iter()
            .position(|r| r.contains("refactor the uploader"))
            .unwrap();
        let other_y = rows
            .iter()
            .position(|r| r.contains("fix the auth middleware"))
            .unwrap();
        assert!(pinned_y < other_y);
    }

    #[test]
    fn ages_are_colored_by_recency() {
        let app = fixture();
        let buffer = buffer(&app, 160, 30);
        let rows = table(&app, 160, 30);
        let color_of = |title: &str, age: &str| {
            let y = rows
                .iter()
                .position(|r| r.contains(title) && r.contains(age))
                .unwrap();
            let row = &rows[y];
            let byte = row.rfind(age).unwrap();
            let x = row[..byte].chars().count() as u16;
            buffer[(x + 1, y as u16)].fg
        };
        let recent = color_of("fix the auth middleware", " 2m");
        let stale = color_of("write release notes", " 5w");
        assert_ne!(recent, stale, "recent and old ages differ in color");
        assert_eq!(recent, rgb(format::age_color(120.0 / 3600.0)));
    }

    #[test]
    fn preview_is_on_the_right_with_header_and_marked_prompts() {
        let rows = screen(&fixture(), 160, 30);
        let table_x = column(&rows[5], "Sessions");
        let preview_x = column(&rows[5], "Preview");
        assert!(preview_x > table_x, "preview is to the right of the table");

        let head = find(&rows, "claude  fix the auth middleware");
        assert!(column(head, "claude  fix") >= preview_x);
        let meta = &rows[7];
        assert!(
            column(meta, "~/code/api  20") >= preview_x,
            "folder and date line: {meta}"
        );
        assert!(column(find(&rows, "» fix the auth middleware"), "» fix") >= preview_x);
        assert!(find(&rows, "● Working on it.").contains('●'));
        assert!(find(&rows, "» and add tests").contains('»'));
    }

    #[test]
    fn user_prompts_and_replies_have_different_colors_in_the_preview() {
        let app = fixture();
        let buffer = buffer(&app, 160, 30);
        let rows = screen(&app, 160, 30);
        let fg_at = |needle: &str| {
            let y = rows.iter().position(|r| r.contains(needle)).unwrap();
            let x = column(&rows[y], needle) as u16;
            buffer[(x + 2, y as u16)].fg
        };
        assert_eq!(fg_at("» and add tests"), THEME.user_text);
        assert_eq!(fg_at("● Tests added."), THEME.assistant_text);
    }

    #[test]
    fn medium_width_drops_the_directory_but_keeps_the_preview() {
        let rows = screen(&fixture(), 120, 30);
        let table_rows = table(&fixture(), 120, 30);
        let header = find(&table_rows, "Agent");
        assert!(!header.contains("Directory"), "{header}");
        assert!(header.contains("Turns") && header.contains("Age"));
        assert!(rows[5].contains("Preview"));
    }

    #[test]
    fn narrow_width_drops_the_preview_too() {
        let rows = screen(&fixture(), 80, 30);
        assert!(!rows.iter().any(|r| r.contains("Preview")));
        let header = find(&rows, "Agent");
        assert!(!header.contains("Directory"));
        assert!(find(&rows, "fix the auth middleware").contains("▸ claude"));
    }

    #[test]
    fn preview_can_be_switched_off() {
        let mut app = fixture();
        app.set_preview_for_test(false);
        let rows = screen(&app, 160, 30);
        assert!(!rows.iter().any(|r| r.contains("Preview")));
        assert!(find(&rows, "Agent").contains("Directory"));
    }

    #[test]
    fn footer_puts_each_key_in_a_highlighted_chip() {
        let footer_keys = keys::footer_keys(true, true, false);
        let line = footer_line(&footer_keys, None, 160);
        for (i, (key, action)) in footer_keys.iter().enumerate() {
            let chip = line
                .spans
                .iter()
                .position(|s| s.content.trim() == *key)
                .unwrap_or_else(|| panic!("no chip for {key}"));
            let style = line.spans[chip].style;
            if i == 0 {
                assert_eq!(style.bg, Some(THEME.accent), "the main action stands out");
                assert_eq!(style.fg, Some(THEME.key_primary_fg));
            } else {
                assert_eq!(style.bg, Some(THEME.key_bg));
                assert_eq!(style.fg, Some(THEME.key_fg));
            }
            assert_eq!(line.spans[chip + 1].content.trim(), *action);
            assert!(line.spans[chip + 1].style.bg.is_none(), "labels are plain");
        }
        let rows = screen(&fixture(), 160, 30);
        let last = rows.last().unwrap();
        assert!(last.contains("Enter  resume"), "{last}");
        assert!(last.contains("Esc  quit"), "{last}");
    }

    #[test]
    fn chips_and_hints_do_not_use_the_terminals_own_grays() {
        // Those change with the terminal theme; ours are fixed colors.
        for color in [THEME.key_bg, THEME.key_fg, THEME.muted, THEME.secondary] {
            assert!(matches!(color, Color::Rgb(..)), "{color:?}");
        }
        let Color::Rgb(r, g, b) = THEME.key_bg else {
            unreachable!()
        };
        assert!(r < 90 && g < 90 && b < 90, "chips are dark, not light gray");
    }

    #[test]
    fn footer_shows_pin_and_rename_and_esc_follows_the_search() {
        let rows = screen(&fixture(), 160, 30);
        let last = rows.last().unwrap();
        for chip in [
            "Enter  resume",
            "Ctrl+F  fork",
            "Ctrl+T  pin",
            "Ctrl+R  rename",
            "Ctrl+O  more",
            "F1  help",
            "Esc  quit",
        ] {
            assert!(last.contains(chip), "{chip} missing: {last}");
        }
        for gone in ["Tab  agent", "Ctrl+P  preview"] {
            assert!(!last.contains(gone), "{gone} moved to the help: {last}");
        }

        let mut app = fixture();
        app.set_query_for_test("auth");
        let rows = screen(&app, 160, 30);
        assert!(rows.last().unwrap().contains("Esc  clear"));

        let mut pinned = fixture();
        pinned.mark_for_test("a", AgentId::Claude, true, false);
        let rows = screen(&pinned, 160, 30);
        assert!(rows.last().unwrap().contains("Ctrl+T  unpin"));
    }

    #[test]
    fn footer_fits_narrow_screens_by_dropping_chips() {
        let rows = screen(&fixture(), 100, 30);
        let last = rows.last().unwrap();
        for chip in ["Ctrl+F  fork", "Ctrl+T  pin", "Ctrl+R  rename", "Esc  quit"] {
            assert!(last.contains(chip), "all chips fit 100 columns: {last}");
        }
        let rows = screen(&fixture(), 70, 30);
        let last = rows.last().unwrap();
        assert!(
            last.contains("Enter  resume") && last.contains("Esc  quit"),
            "{last}"
        );
        assert!(last.contains("F1  help"), "{last}");
        assert!(!last.contains("fork"), "{last}");
    }

    fn ctrl(c: char) -> ratatui::crossterm::event::KeyEvent {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn actions_menu_lists_each_action_with_its_shortcut() {
        let mut app = fixture();
        app.open_actions_for_test();
        let rows = screen(&app, 160, 30);
        find(&rows, "╭ Actions ");
        let mut last_y = 0;
        for (label, shortcut) in [
            ("Pin to the top", "Ctrl+T"),
            ("Rename", "Ctrl+R"),
            ("Fork into a new session", "Ctrl+F"),
            ("Copy the resume command", "Ctrl+Y"),
            ("Hide from hopback", "Ctrl+X"),
        ] {
            let y = rows
                .iter()
                .position(|r| r.contains(label) && r.contains('│'))
                .unwrap_or_else(|| panic!("{label} not in the menu"));
            assert!(rows[y].contains(shortcut), "{}", rows[y]);
            assert!(column(&rows[y], shortcut) > column(&rows[y], label));
            assert!(y > last_y, "{label} is out of order");
            last_y = y;
        }
        assert!(
            find(&rows, "Pin to the top").contains("▸ Pin"),
            "first is selected"
        );
        find(&rows, "↑↓ move   Enter choose   Esc close");

        let mut pinned = fixture();
        pinned.mark_for_test("a", AgentId::Claude, true, false);
        pinned.open_actions_for_test();
        let rows = screen(&pinned, 160, 30);
        find(&rows, "▸ Unpin");
    }

    /// The top border row of the open pop-up and the color it is drawn in.
    fn popup_frame(app: &App, title: &str) -> (Vec<String>, Color) {
        let buffer = buffer(app, 160, 40);
        let rows = screen(app, 160, 40);
        let needle = format!("╭ {title} ");
        let y = rows
            .iter()
            .position(|r| r.contains(&needle))
            .unwrap_or_else(|| panic!("no {title} pop-up:\n{}", rows.join("\n")));
        let x = column(&rows[y], &needle);
        let color = buffer[(x as u16, y as u16)].style().fg.unwrap();
        (rows, color)
    }

    #[test]
    fn every_pop_up_shares_one_frame_and_hint_style() {
        // Fork: a claude session can fork natively or into codex.
        let mut fork = fixture();
        fork.press_for_test(ctrl('f'));
        let (rows, color) = popup_frame(&fork, "Fork");
        assert_eq!(color, THEME.accent);
        find(&rows, "Fork this claude session as");
        find(&rows, "▸ Native claude fork");
        find(&rows, "  Translated codex fork");
        find(&rows, "↑↓ move   Enter choose   Esc close");

        let mut rename = fixture();
        rename.press_for_test(ctrl('r'));
        let (rows, color) = popup_frame(&rename, "Rename");
        assert_eq!(color, THEME.accent);
        find(&rows, "› fix the auth middleware");
        find(&rows, "Enter save   Esc cancel   Ctrl+U clear");

        let mut actions = fixture();
        actions.open_actions_for_test();
        assert_eq!(popup_frame(&actions, "Actions").1, THEME.accent);

        let mut help = fixture();
        help.open_help_for_test();
        assert_eq!(popup_frame(&help, "Keys").1, THEME.accent);

        // Hiding is the one destructive prompt, so it alone is red.
        let mut hide = fixture();
        hide.press_for_test(ctrl('x'));
        let (rows, color) = popup_frame(&hide, "Hide");
        assert_eq!(color, THEME.error);
        find(&rows, "Hide this session from hopback?");
        find(&rows, "Enter hide   Esc cancel");

        // No pop-up has the old lowercase titles, hints or an em dash.
        for app in [&fork, &rename, &actions, &help, &hide] {
            let text = screen(app, 160, 40).join("\n");
            for old in [
                "fork target",
                "esc cancel",
                "enter choose",
                "—",
                "press a letter",
            ] {
                assert!(!text.contains(old), "{old:?} is still on screen");
            }
        }
    }

    #[test]
    fn the_selected_choice_has_the_same_bar_as_the_selected_session() {
        let mut app = fixture();
        app.press_for_test(ctrl('f'));
        let buffer = buffer(&app, 160, 40);
        let rows = screen(&app, 160, 40);
        let y = rows
            .iter()
            .position(|r| r.contains("▸ Native claude fork"))
            .unwrap();
        let x = column(&rows[y], "Native");
        assert_eq!(
            buffer[(x as u16, y as u16)].style().bg,
            Some(THEME.selected_bg)
        );
        let other = rows
            .iter()
            .position(|r| r.contains("Translated codex fork"))
            .unwrap();
        let x = column(&rows[other], "Translated");
        assert_ne!(
            buffer[(x as u16, other as u16)].style().bg,
            Some(THEME.selected_bg)
        );
    }

    #[test]
    fn rename_pop_up_holds_the_cursor_and_scrolls_long_names() {
        let mut app = fixture();
        app.press_for_test(ctrl('r'));
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        let rows = screen(&app, 160, 40);
        let y = rows
            .iter()
            .position(|r| r.contains("› fix the auth"))
            .unwrap();
        assert_eq!(cursor.y as usize, y, "the cursor is in the pop-up");
        let end = column(&rows[y], "› fix") + "› fix the auth middleware".chars().count();
        assert_eq!(cursor.x as usize, end);

        let mut long = App::for_test(
            vec![sess("z", AgentId::Claude, &"word ".repeat(30), "/w", 60)],
            "",
            Filters::default(),
        );
        long.press_for_test(ctrl('r'));
        let rows = screen(&long, 160, 40);
        let row = find(&rows, "› ");
        let frame = rows.iter().find(|r| r.contains("╭ Rename ")).unwrap();
        assert!(
            row.trim_end().chars().count() <= frame.trim_end().chars().count(),
            "the name stays inside the frame: {row}"
        );
    }

    #[test]
    fn filter_keywords_are_colored_in_the_search_line() {
        let text = "api -agent:claude date:nope dir:src agent:!codex,agy";
        let styles = query_styles(text);
        let at = |needle: &str| {
            let byte = text.find(needle).unwrap();
            styles[text[..byte].chars().count()]
        };
        assert_eq!(at("api").fg, Some(THEME.foreground));
        assert_eq!(at("-agent").fg, Some(THEME.error), "the minus sign");
        assert_eq!(at("agent:claude").fg, Some(THEME.info));
        assert_eq!(at("claude").fg, Some(agent_color(AgentId::Claude)));
        // An unknown value: keyword and value in red, the value struck out.
        assert_eq!(at("date:nope").fg, Some(THEME.error));
        assert_eq!(at("nope").fg, Some(THEME.error));
        assert!(at("nope").add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(at("dir:src").fg, Some(THEME.info));
        assert_eq!(at("src").fg, Some(THEME.success));
        assert_eq!(at("!codex").fg, Some(THEME.error), "the exclamation mark");
        assert_eq!(at("codex").fg, Some(agent_color(AgentId::Codex)));
        assert_eq!(at("agy").fg, Some(agent_color(AgentId::Antigravity)));

        // A keyword with no value yet is still plain text.
        assert!(query_styles("agent:")
            .iter()
            .all(|style| style.fg == Some(THEME.foreground)));

        // The spans put the text back together unchanged.
        let joined: String = query_spans(text, 0, text.chars().count())
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(joined, text);
        let window: String = query_spans(text, 4, 13)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(window, "-agent:claude");
    }

    #[test]
    fn the_search_line_on_screen_carries_the_colors() {
        let mut app = fixture();
        app.set_query_for_test("agent:codex up");
        let buffer = buffer(&app, 160, 30);
        let rows = screen(&app, 160, 30);
        let x = column(&rows[2], "agent:codex") as u16;
        assert_eq!(buffer[(x, 2)].style().fg, Some(THEME.info));
        assert_eq!(
            buffer[(x + 6, 2)].style().fg,
            Some(agent_color(AgentId::Codex))
        );
        assert_eq!(buffer[(x + 12, 2)].style().fg, Some(THEME.foreground));
    }

    #[test]
    fn a_suggested_value_is_shown_dimmed_after_the_cursor() {
        let mut app = fixture();
        app.set_query_for_test("agent:co");
        let buffer = buffer(&app, 160, 30);
        let rows = screen(&app, 160, 30);
        assert!(rows[2].contains("› agent:codex"), "{}", rows[2]);
        let x = column(&rows[2], "agent:co") as u16;
        assert_eq!(
            buffer[(x + 8, 2)].style().fg,
            Some(THEME.muted),
            "the d of dex"
        );
        assert_ne!(
            buffer[(x + 7, 2)].style().fg,
            Some(THEME.muted),
            "typed text"
        );

        // Not while the cursor is inside the text.
        app.set_cursor_for_test(3);
        let rows = screen(&app, 160, 30);
        assert!(!rows[2].contains("agent:codex"), "{}", rows[2]);
    }

    #[test]
    fn the_tab_of_a_typed_agent_is_filled_in() {
        let mut app = fixture();
        app.set_query_for_test("agent:codex");
        let line = tabs_line(&app.view());
        let filled: Vec<&str> = line
            .spans
            .iter()
            .filter(|s| s.style.bg.is_some())
            .map(|s| s.content.trim())
            .collect();
        assert_eq!(filled, vec!["codex 1"]);
    }

    #[test]
    fn help_overlay_lists_every_key() {
        let mut app = fixture();
        app.open_help_for_test();
        let rows = screen(&app, 160, 50);
        find(&rows, "Keys");
        for (_, entries) in keys::KEY_HELP {
            for (key, action) in *entries {
                let row = find(&rows, action);
                assert!(row.contains(key), "{key} not beside {action}: {row}");
            }
        }
        find(&rows, "Any key close");

        // A shorter terminal drops the blank lines and still shows it all.
        let rows = screen(&app, 160, 34);
        find(&rows, "Ctrl+C");
        find(&rows, "Any key close");
        // Shorter still: the closing hint stays and says there is more.
        let rows = screen(&app, 160, 24);
        find(&rows, "Any key close   Taller window shows the rest");
    }

    #[test]
    fn cursor_sits_where_the_next_character_goes() {
        let mut app = fixture();
        app.set_query_for_test("auth bug");
        let cursor_at = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            terminal.get_cursor_position().unwrap()
        };
        // Border, padding and the "› " prompt put the text at column 4.
        assert_eq!(cursor_at(&app), Position::new(4 + 8, 2));
        app.set_cursor_for_test(4);
        assert_eq!(cursor_at(&app), Position::new(4 + 4, 2));
        app.set_cursor_for_test(0);
        assert_eq!(cursor_at(&app), Position::new(4, 2));
    }

    #[test]
    fn long_search_text_scrolls_to_keep_the_cursor_in_the_box() {
        let mut app = fixture();
        let long = "word ".repeat(40) + "END";
        app.set_query_for_test(&long);
        let rows = screen(&app, 80, 20);
        assert!(rows[2].contains("END"), "the end is visible: {}", rows[2]);
        assert!(rows[2].ends_with('│'), "the frame is intact: {}", rows[2]);
        app.set_cursor_for_test(0);
        let rows = screen(&app, 80, 20);
        assert!(rows[2].contains("› word word"), "{}", rows[2]);
        assert!(!rows[2].contains("END"));
    }

    #[test]
    fn the_table_keeps_its_scroll_position_between_frames() {
        let sessions: Vec<Session> = (0..80)
            .map(|i| {
                sess(
                    &format!("s{i}"),
                    AgentId::Claude,
                    &format!("title {i:02}"),
                    "/w",
                    60 + i,
                )
            })
            .collect();
        let mut app = App::for_test(sessions, "", Filters::default());
        app.finish_refresh_for_test(0, 1);
        let rows = screen(&app, 120, 20);
        assert!(rows.iter().any(|r| r.contains("▸ claude    title 00")));
        assert_eq!(app.view().table_offset, 0);

        app.select_for_test(40);
        screen(&app, 120, 20);
        let offset = app.view().table_offset;
        assert!(
            offset > 0 && offset <= 40,
            "scrolled to show row 40: {offset}"
        );

        // Moving up one row inside the window does not scroll the table.
        app.select_for_test(39);
        screen(&app, 120, 20);
        assert_eq!(app.view().table_offset, offset);
    }

    #[test]
    fn footer_shows_a_toast_on_the_right() {
        let line = footer_line(&keys::footer_keys(true, true, false), Some("pinned"), 160);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("pinned"));
        assert_eq!(text.chars().count(), 159);
    }

    #[test]
    fn empty_index_says_so() {
        let mut app = App::for_test(vec![], "", Filters::default());
        app.finish_refresh_for_test(0, 3);
        let rows = screen(&app, 120, 20);
        assert!(rows.iter().any(|r| r.contains("No sessions found")));
        assert!(rows.iter().any(|r| r.contains("No session selected")));
        assert!(rows[0].contains("0 shown / 0 indexed"));
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for (w, h) in [(1, 1), (10, 3), (30, 6), (60, 8)] {
            let _ = screen(&fixture(), w, h);
        }
    }

    #[test]
    fn highlight_marks_matched_terms() {
        let line = highlight("authentication middleware", "auth");
        let texts: Vec<String> = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(texts.iter().any(|t| t.eq_ignore_ascii_case("auth")));
    }

    #[test]
    fn highlight_no_query_is_plain() {
        let line = highlight("hello world", "");
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn highlight_does_not_panic_on_length_changing_multibyte() {
        for (text, q) in [
            ("ẞẞẞ match", "match"),
            ("İx", "i"),
            ("aİbİc match", "i"),
            ("\u{212A}elvin match", "match"),
        ] {
            let line = highlight(text, q);

            let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(
                joined, text,
                "spans must reconstruct the input for {text:?}"
            );
        }
    }

    #[test]
    fn highlight_reconstructs_and_marks_ascii() {
        let line = highlight("authentication middleware", "auth mid");
        let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "authentication middleware");

        let styled: Vec<&str> = line
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(THEME.match_bg))
            .map(|s| s.content.as_ref())
            .collect();
        assert!(styled.iter().any(|t| t.eq_ignore_ascii_case("auth")));
        assert!(styled.iter().any(|t| t.eq_ignore_ascii_case("mid")));
    }

    #[test]
    fn floor_char_boundary_is_safe_on_multibyte() {
        let s = "αβγδε";
        let b = floor_char_boundary(s, 3);
        assert!(s.is_char_boundary(b));
        assert!(b <= 3);
    }

    #[test]
    fn preview_line_marks_user_and_assistant() {
        let mut role = Role::Other;
        let u = preview_line("» hello there", "", AgentId::Claude, &mut role);
        assert_eq!(u.spans[0].content, "» ");
        assert!(role == Role::User);
        let cont = preview_line("second line of the prompt", "", AgentId::Claude, &mut role);
        assert_eq!(cont.spans[1].style.fg, Some(THEME.user_text));
        let a = preview_line("  on it", "", AgentId::Claude, &mut role);
        assert_eq!(a.spans[0].content, "● ");
        assert!(role == Role::Assistant);
    }

    #[test]
    fn preview_messages_are_separated_by_one_blank_line() {
        let rows = screen(&fixture(), 160, 30);
        let prompt = rows
            .iter()
            .position(|r| r.contains("» fix the auth middleware"))
            .unwrap();
        let reply = rows
            .iter()
            .position(|r| r.contains("● Working on it."))
            .unwrap();
        assert_eq!(reply, prompt + 2);
    }

    #[test]
    fn preview_starts_at_a_message_boundary_near_the_match() {
        let mut content = String::new();
        for i in 0..200 {
            content.push_str(&format!("» question number {i}\n\n  answer number {i}\n\n"));
        }
        content.push_str("» the needle is here\n\n  found it");
        let (snippet, cut) = preview_snippet(&content, "needle");
        assert!(cut);
        assert!(snippet.contains("the needle is here"));
        assert!(
            snippet.starts_with("» ") || snippet.starts_with("  "),
            "starts on a message"
        );
        let (all, cut) = preview_snippet(&content, "");
        assert!(!cut);
        assert!(all.starts_with("» question number 0"));
    }
}
